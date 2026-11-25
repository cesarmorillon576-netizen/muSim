mod ia;
mod spice;

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints, VLine};

struct App {
    netlist: String,
    nodo: String,
    puntos: Vec<[f64; 2]>,
    corte: Option<f64>,
    mensaje: String,
    peticion: String,
    pidiendo: Option<(Receiver<Result<ia::Respuesta, String>>, Instant)>,
    respuesta_ia: String,
    pasos: Vec<String>,
}

fn hz(f: f64) -> String {
    match f {
        f if f >= 1e6 => format!("{:.3} MHz", f / 1e6),
        f if f >= 1e3 => format!("{:.2} kHz", f / 1e3),
        f => format!("{f:.1} Hz"),
    }
}

impl App {
    fn nueva() -> Self {
        let mut app = Self {
            netlist: spice::EJEMPLO.to_string(),
            nodo: "salida".to_string(),
            puntos: Vec::new(),
            corte: None,
            mensaje: String::new(),
            peticion: String::new(),
            pidiendo: None,
            respuesta_ia: String::new(),
            pasos: Vec::new(),
        };
        app.simular();
        app
    }

    fn simular(&mut self) {
        match spice::simular(&self.netlist, &self.nodo) {
            Ok(puntos) => {
                self.corte = spice::corte(&puntos);
                let medido = self
                    .corte
                    .map_or("sin caída de −3 dB entre 1 Hz y 1 MHz".to_string(), hz);
                let calculado = spice::corte_rc(&self.netlist).map_or("—".to_string(), hz);
                self.mensaje = format!("Corte medido: {medido} · calculado (1/2πRC): {calculado}");
                self.puntos = puntos;
            }
            Err(error) => {
                self.mensaje = error;
                self.puntos.clear();
                self.corte = None;
            }
        }
    }

    fn pedir(&mut self) {
        let (tx, rx) = mpsc::channel();
        let (netlist, peticion) = (self.netlist.clone(), self.peticion.clone());
        std::thread::spawn(move || {
            let _ = tx.send(ia::pedir(&netlist, &peticion));
        });
        self.pidiendo = Some((rx, Instant::now()));
        self.respuesta_ia.clear();
        self.pasos.clear();
    }

    fn revisar_ia(&mut self, ctx: &egui::Context) {
        let Some(estado) = self.pidiendo.as_ref().map(|(rx, _)| rx.try_recv()) else {
            return;
        };
        match estado {
            Err(TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(250)),
            Err(TryRecvError::Disconnected) => {
                self.pidiendo = None;
                self.respuesta_ia = "La consulta a Qwen se cerró sin respuesta".into();
            }
            Ok(Ok(respuesta)) => {
                self.pidiendo = None;
                self.netlist = respuesta.netlist;
                self.respuesta_ia = if respuesta.explicacion.is_empty() {
                    "Listo".into()
                } else {
                    respuesta.explicacion
                };
                self.pasos = respuesta.pasos;
                self.simular();
            }
            Ok(Err(error)) => {
                self.pidiendo = None;
                self.respuesta_ia = error;
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.revisar_ia(ctx);
        let libre = self.pidiendo.is_none();
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("µSim");
            ui.add_space(6.0);
            ui.add_enabled(
                libre,
                egui::TextEdit::multiline(&mut self.netlist)
                    .code_editor()
                    .desired_rows(6)
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                ui.label("Nodo de salida");
                ui.add(egui::TextEdit::singleline(&mut self.nodo).desired_width(120.0));
                if ui
                    .add_enabled(libre, egui::Button::new("Simular"))
                    .clicked()
                {
                    self.simular();
                }
            });
            ui.label(&self.mensaje);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Pídele a Qwen");
                ui.add_enabled(
                    libre,
                    egui::TextEdit::singleline(&mut self.peticion)
                        .hint_text("Por ejemplo: hazlo pasa bajos con corte en 500 Hz")
                        .desired_width(420.0),
                );
                let puede = libre && !self.peticion.trim().is_empty();
                if ui.add_enabled(puede, egui::Button::new("Pedir")).clicked() {
                    self.pedir();
                }
            });
            if let Some((_, inicio)) = &self.pidiendo {
                ui.label(format!(
                    "Qwen está pensando… {} s",
                    inicio.elapsed().as_secs()
                ));
            } else if !self.respuesta_ia.is_empty() {
                ui.label(&self.respuesta_ia);
                for paso in &self.pasos {
                    ui.small(paso);
                }
            }
            ui.add_space(6.0);
            let curva = Line::new(PlotPoints::from(
                self.puntos
                    .iter()
                    .map(|[f, db]| [f.log10(), *db])
                    .collect::<Vec<_>>(),
            ))
            .name(format!("V({})", self.nodo));
            Plot::new("bode")
                .x_axis_label("Frecuencia")
                .y_axis_label("Magnitud (dB)")
                .x_axis_formatter(|marca, _| hz(10f64.powf(marca.value)))
                .show(ui, |plot| {
                    plot.line(curva);
                    if let Some(f) = self.corte {
                        plot.vline(VLine::new(f.log10()).name("−3 dB"));
                    }
                });
        });
    }
}

fn main() -> eframe::Result {
    let opciones = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([900.0, 680.0]),
        ..Default::default()
    };
    eframe::run_native("µSim", opciones, Box::new(|_| Ok(Box::new(App::nueva()))))
}
