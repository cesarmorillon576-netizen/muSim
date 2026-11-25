use std::f64::consts::PI;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static SIMULACIONES: AtomicUsize = AtomicUsize::new(0);

pub const EJEMPLO: &str = "V1 entrada 0 AC 1\nR1 entrada salida 1k\nC1 salida 0 159n";

pub fn valor(texto: &str) -> Option<f64> {
    let t = texto.trim().to_lowercase();
    let (numero, resto) = t
        .char_indices()
        .map(|(i, _)| i)
        .chain([t.len()])
        .rev()
        .find_map(|i| t[..i].parse::<f64>().ok().map(|n| (n, &t[i..])))?;
    let escala = if resto.starts_with("meg") {
        1e6
    } else {
        match resto.chars().next() {
            Some('f') => 1e-15,
            Some('p') => 1e-12,
            Some('n') => 1e-9,
            Some('u' | 'µ') => 1e-6,
            Some('m') => 1e-3,
            Some('k') => 1e3,
            Some('g') => 1e9,
            Some('t') => 1e12,
            _ => 1.0,
        }
    };
    Some(numero * escala)
}

pub fn corte(puntos: &[[f64; 2]]) -> Option<f64> {
    let objetivo = puntos.first()?[1] - 10.0 * 2f64.log10();
    puntos
        .windows(2)
        .find(|w| w[0][1] >= objetivo && w[1][1] < objetivo)
        .map(|w| {
            let t = (w[0][1] - objetivo) / (w[0][1] - w[1][1]);
            w[0][0] * (w[1][0] / w[0][0]).powf(t)
        })
}

pub fn corte_rc(netlist: &str) -> Option<f64> {
    let (mut r, mut c) = (Vec::new(), Vec::new());
    for linea in netlist.lines() {
        let partes: Vec<&str> = linea.split_whitespace().collect();
        if partes.len() < 4 {
            continue;
        }
        match partes[0].chars().next()?.to_ascii_lowercase() {
            'r' => r.push(valor(partes[3])?),
            'c' => c.push(valor(partes[3])?),
            _ => {}
        }
    }
    (r.len() == 1 && c.len() == 1).then(|| 1.0 / (2.0 * PI * r[0] * c[0]))
}

pub fn simular(netlist: &str, nodo: &str) -> Result<Vec<[f64; 2]>, String> {
    if nodo.is_empty() || !nodo.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("El nodo de salida solo puede tener letras, números y guion bajo".into());
    }
    let dir = std::env::temp_dir().join(format!(
        "musim-{}-{}",
        std::process::id(),
        SIMULACIONES.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let circuito = dir.join("circuito.cir");
    let datos = dir.join("datos.txt");
    let _ = std::fs::remove_file(&datos);
    let cuerpo: String = netlist
        .lines()
        .filter(|l| !l.trim().eq_ignore_ascii_case(".end"))
        .map(|l| format!("{l}\n"))
        .collect();
    let texto = format!(
        "musim\n{cuerpo}.control\nac dec 100 1 1meg\nwrdata {} vdb({nodo})\n.endc\n.end\n",
        datos.display()
    );
    std::fs::write(&circuito, texto).map_err(|e| e.to_string())?;
    let salida = Command::new("ngspice")
        .arg("-b")
        .arg(&circuito)
        .output()
        .map_err(|_| "No encuentro ngspice; instálalo con: sudo apt install ngspice".to_string())?;
    let leido = std::fs::read_to_string(&datos);
    let _ = std::fs::remove_dir_all(&dir);
    let puntos: Vec<[f64; 2]> = leido
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut columnas = l.split_whitespace().map(|x| x.parse::<f64>());
            Some([columnas.next()?.ok()?, columnas.next()?.ok()?])
        })
        .collect();
    if puntos.is_empty() {
        let log = String::from_utf8_lossy(&salida.stdout) + String::from_utf8_lossy(&salida.stderr);
        let errores: Vec<&str> = log
            .lines()
            .filter(|l| l.to_lowercase().contains("error"))
            .collect();
        return Err(if errores.is_empty() {
            "ngspice no devolvió datos; revisa la netlist y el nodo de salida".into()
        } else {
            errores.join("\n")
        });
    }
    Ok(puntos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lee_sufijos_de_spice() {
        let casos = [
            ("1k", 1e3),
            ("159n", 159e-9),
            ("2.2u", 2.2e-6),
            ("2.2µF", 2.2e-6),
            ("1meg", 1e6),
            ("4m7", 4e-3),
            ("100nF", 100e-9),
            ("1e3", 1e3),
            ("47", 47.0),
        ];
        for (texto, esperado) in casos {
            let v = valor(texto).unwrap();
            assert!(
                (v - esperado).abs() <= esperado.abs() * 1e-9,
                "{texto}: {v}"
            );
        }
        assert!(valor("abc").is_none());
    }

    #[test]
    fn corte_interpola_en_escala_logaritmica() {
        let puntos = [[100.0, 0.0], [1000.0, -2.0], [10000.0, -4.0]];
        let f = corte(&puntos).unwrap();
        assert!((f - 1000.0 * 10f64.powf(0.50515)).abs() < 1.0, "{f}");
        assert!(corte(&[[1.0, 0.0], [2.0, -1.0]]).is_none());
    }

    #[test]
    fn calcula_el_corte_de_un_rc() {
        let f = corte_rc(EJEMPLO).unwrap();
        assert!((f - 1000.97).abs() < 0.1, "{f}");
        assert!(corte_rc("V1 a 0 AC 1\nR1 a b 1k").is_none());
    }

    #[test]
    fn ngspice_coincide_con_la_formula() {
        let puntos = simular(EJEMPLO, "salida").unwrap();
        let medido = corte(&puntos).unwrap();
        let calculado = corte_rc(EJEMPLO).unwrap();
        assert!(
            (medido / calculado - 1.0).abs() < 0.01,
            "{medido} vs {calculado}"
        );
    }

    #[test]
    fn nodo_inexistente_da_error_legible() {
        assert!(simular(EJEMPLO, "nada").is_err());
        assert!(simular(EJEMPLO, "salida\nquit").is_err());
    }
}
