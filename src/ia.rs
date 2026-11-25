use std::f64::consts::PI;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::spice;

const MODELO: &str = "qwen3.5:4b-q4_K_M";
const URL: &str = "http://127.0.0.1:11434";
const VUELTAS: usize = 6;

const SISTEMA: &str = "Eres el asistente de µSim, un simulador de circuitos. Recibes el circuito como \
netlist SPICE: cada línea tiene nombre, nodos y valor. No calcules valores de memoria: usa las \
herramientas para calcular y para cambiar componentes. Cuando termines, explica en una o dos frases \
en español qué cambiaste.";

pub struct Respuesta {
    pub netlist: String,
    pub explicacion: String,
    pub pasos: Vec<String>,
}

fn herramientas() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "calcular_rc_pasa_bajos",
                "description": "Calcula la resistencia de un filtro RC pasa bajos para una frecuencia de corte, conservando el capacitor del circuito.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "frecuencia_hz": { "type": "number", "description": "Frecuencia de corte en Hz" }
                    },
                    "required": ["frecuencia_hz"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "cambiar_valor",
                "description": "Cambia el valor de un componente de la netlist.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "componente": { "type": "string", "description": "Nombre del componente, por ejemplo R1" },
                        "valor": { "type": "string", "description": "Valor en notación SPICE, por ejemplo 3.18k o 100n" }
                    },
                    "required": ["componente", "valor"]
                }
            }
        }
    ])
}

pub fn formato(v: f64) -> String {
    let (escala, sufijo) = [
        (1e6, "meg"),
        (1e3, "k"),
        (1.0, ""),
        (1e-3, "m"),
        (1e-6, "u"),
        (1e-9, "n"),
        (1e-12, "p"),
    ]
    .into_iter()
    .find(|(e, _)| v.abs() >= *e * 0.9995)
    .unwrap_or((1e-12, "p"));
    let n = v / escala;
    let decimales = if n.abs() < 10.0 {
        2
    } else if n.abs() < 100.0 {
        1
    } else {
        0
    };
    let texto = format!("{n:.decimales$}");
    let texto = if texto.contains('.') {
        texto.trim_end_matches('0').trim_end_matches('.')
    } else {
        &texto
    };
    format!("{texto}{sufijo}")
}

fn numero(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(spice::valor))
}

pub fn calcular_rc(netlist: &str, frecuencia: f64) -> Result<String, String> {
    if frecuencia.is_nan() || frecuencia <= 0.0 {
        return Err("la frecuencia debe ser mayor que cero".into());
    }
    let c = netlist
        .lines()
        .find_map(|l| {
            let p: Vec<&str> = l.split_whitespace().collect();
            (p.len() >= 4 && p[0].to_ascii_lowercase().starts_with('c'))
                .then(|| spice::valor(p[3]))
                .flatten()
        })
        .unwrap_or(100e-9);
    let r = 1.0 / (2.0 * PI * frecuencia * c);
    Ok(json!({ "r": formato(r), "c": formato(c) }).to_string())
}

pub fn cambiar_valor(netlist: &str, nombre: &str, valor: &str) -> Result<String, String> {
    spice::valor(valor).ok_or(format!("«{valor}» no es un valor válido"))?;
    let mut encontrado = false;
    let lineas: Vec<String> = netlist
        .lines()
        .map(|l| {
            let mut p: Vec<&str> = l.split_whitespace().collect();
            if p.len() >= 4 && p[0].eq_ignore_ascii_case(nombre) {
                p[3] = valor;
                encontrado = true;
                p.join(" ")
            } else {
                l.to_string()
            }
        })
        .collect();
    if !encontrado {
        return Err(format!("no existe el componente {nombre}"));
    }
    Ok(lineas.join("\n"))
}

pub fn conversar(
    netlist: &str,
    peticion: &str,
    mut chat: impl FnMut(&[Value]) -> Result<Value, String>,
) -> Result<Respuesta, String> {
    let mut actual = netlist.to_string();
    let mut pasos = Vec::new();
    let mut mensajes = vec![
        json!({ "role": "system", "content": SISTEMA }),
        json!({ "role": "user", "content": format!("Circuito:\n{netlist}\n\nPetición: {peticion}") }),
    ];
    for _ in 0..VUELTAS {
        let mensaje = chat(&mensajes)?;
        let llamadas = mensaje["tool_calls"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        mensajes.push(mensaje.clone());
        if llamadas.is_empty() {
            return Ok(Respuesta {
                netlist: actual,
                explicacion: mensaje["content"].as_str().unwrap_or("").trim().to_string(),
                pasos,
            });
        }
        for llamada in llamadas {
            let nombre = llamada["function"]["name"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let args = &llamada["function"]["arguments"];
            let resultado = match nombre.as_str() {
                "calcular_rc_pasa_bajos" => numero(&args["frecuencia_hz"])
                    .ok_or("falta frecuencia_hz".to_string())
                    .and_then(|f| calcular_rc(&actual, f)),
                "cambiar_valor" => {
                    let valor = args["valor"]
                        .as_str()
                        .map(String::from)
                        .or_else(|| args["valor"].as_f64().map(formato))
                        .unwrap_or_default();
                    cambiar_valor(&actual, args["componente"].as_str().unwrap_or(""), &valor).map(
                        |nueva| {
                            actual = nueva;
                            "listo".to_string()
                        },
                    )
                }
                otro => Err(format!("no existe la herramienta {otro}")),
            };
            let texto = resultado.unwrap_or_else(|e| format!("error: {e}"));
            pasos.push(format!("{nombre}({args}) → {texto}"));
            mensajes.push(json!({ "role": "tool", "tool_name": nombre, "content": texto }));
        }
    }
    Err(format!("Qwen no terminó después de {VUELTAS} vueltas"))
}

struct Servidor(Option<Child>);

impl Servidor {
    fn responde() -> bool {
        ureq::get(&format!("{URL}/api/version"))
            .timeout(Duration::from_secs(2))
            .call()
            .is_ok()
    }

    fn asegurar() -> Result<Self, String> {
        if Self::responde() {
            return Ok(Self(None));
        }
        let hijo = Command::new("ollama")
            .arg("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "No encuentro Ollama; instálalo desde ollama.com".to_string())?;
        let servidor = Self(Some(hijo));
        let limite = Instant::now() + Duration::from_secs(15);
        while Instant::now() < limite {
            if Self::responde() {
                return Ok(servidor);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err("Ollama no arrancó a tiempo".into())
    }
}

impl Drop for Servidor {
    fn drop(&mut self) {
        if let Some(hijo) = &mut self.0 {
            let _ = Command::new("kill").arg(hijo.id().to_string()).status();
            let _ = hijo.wait();
        }
    }
}

pub fn pedir(netlist: &str, peticion: &str) -> Result<Respuesta, String> {
    let _servidor = Servidor::asegurar()?;
    let agente = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(600))
        .build();
    let respuesta = conversar(netlist, peticion, |mensajes| {
        let cuerpo = json!({
            "model": MODELO,
            "messages": mensajes,
            "tools": herramientas(),
            "stream": false,
            "think": false,
            "keep_alive": "2m",
            "options": { "num_ctx": 4096, "num_thread": 3, "temperature": 0.2 }
        });
        let v: Value = agente
            .post(&format!("{URL}/api/chat"))
            .send_json(cuerpo)
            .map_err(|e| format!("Ollama: {e}"))?
            .into_json()
            .map_err(|e| format!("respuesta de Ollama ilegible: {e}"))?;
        Ok(v["message"].clone())
    });
    let _ = agente
        .post(&format!("{URL}/api/generate"))
        .send_json(json!({ "model": MODELO, "keep_alive": 0 }));
    respuesta
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formato_en_notacion_spice() {
        assert_eq!(formato(3183.1), "3.18k");
        assert_eq!(formato(1000.0), "1k");
        assert_eq!(formato(159e-9), "159n");
        assert_eq!(formato(2.2e-6), "2.2u");
        assert_eq!(formato(47.0), "47");
        assert_eq!(formato(999.9), "1k");
        assert_eq!(formato(1.5e6), "1.5meg");
    }

    #[test]
    fn calcula_r_conservando_el_capacitor() {
        let r: Value = serde_json::from_str(&calcular_rc(spice::EJEMPLO, 500.0).unwrap()).unwrap();
        assert_eq!(r["c"], "159n");
        assert_eq!(r["r"], "2k");
        assert!(calcular_rc(spice::EJEMPLO, 0.0).is_err());
    }

    #[test]
    fn cambia_solo_el_componente_pedido() {
        let nueva = cambiar_valor(spice::EJEMPLO, "r1", "2k").unwrap();
        assert!(nueva.contains("R1 entrada salida 2k"));
        assert!(nueva.contains("C1 salida 0 159n"));
        assert!(cambiar_valor(spice::EJEMPLO, "R9", "2k").is_err());
        assert!(cambiar_valor(spice::EJEMPLO, "R1", "mucho").is_err());
    }

    #[test]
    fn el_ciclo_ejecuta_las_herramientas_que_pide_el_modelo() {
        let guion = [
            json!({ "role": "assistant", "content": "", "tool_calls": [
                { "function": { "name": "calcular_rc_pasa_bajos", "arguments": { "frecuencia_hz": 500 } } }
            ]}),
            json!({ "role": "assistant", "content": "", "tool_calls": [
                { "function": { "name": "cambiar_valor", "arguments": { "componente": "R1", "valor": "2k" } } }
            ]}),
            json!({ "role": "assistant", "content": "Cambié R1 a 2k para cortar en 500 Hz." }),
        ];
        let mut vistos = Vec::new();
        let mut turno = guion.iter();
        let r = conversar(spice::EJEMPLO, "corte en 500 Hz", |m| {
            vistos.push(m.len());
            Ok(turno.next().unwrap().clone())
        })
        .unwrap();
        assert!(r.netlist.contains("R1 entrada salida 2k"));
        assert_eq!(r.pasos.len(), 2);
        assert_eq!(r.explicacion, "Cambié R1 a 2k para cortar en 500 Hz.");
        assert_eq!(vistos, [2, 4, 6]);
    }

    #[test]
    fn herramienta_desconocida_se_le_reporta_al_modelo() {
        let guion = [
            json!({ "role": "assistant", "content": "", "tool_calls": [
                { "function": { "name": "volar", "arguments": {} } }
            ]}),
            json!({ "role": "assistant", "content": "No pude." }),
        ];
        let mut turno = guion.iter();
        let r = conversar(spice::EJEMPLO, "algo", |_| {
            Ok(turno.next().unwrap().clone())
        })
        .unwrap();
        assert!(r.pasos[0].contains("error: no existe la herramienta volar"));
        assert_eq!(r.netlist, spice::EJEMPLO);
    }

    #[test]
    #[ignore]
    fn qwen_ajusta_el_corte_de_verdad() {
        let r = pedir(spice::EJEMPLO, "Hazlo pasa bajos con corte en 500 Hz").unwrap();
        println!("{}\n{}\n{:#?}", r.netlist, r.explicacion, r.pasos);
        let puntos = spice::simular(&r.netlist, "salida").unwrap();
        let corte = spice::corte(&puntos).unwrap();
        assert!((corte / 500.0 - 1.0).abs() < 0.05, "corte {corte}");
    }
}
