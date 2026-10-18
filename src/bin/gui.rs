#![windows_subsystem = "windows"]

use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::fs;
use url::Url;

use chromatic_ez_firmware as lib;
use lib::{
    count_chromatics, get_firmware_options, flash_fpga, flash_mcu, verify_hash,
    Target, HashOutcome, Firmware, Mode
};

#[derive(Debug)]
enum TaskMessage {
    Progress(String, f32 /* 0.0 -> 1.0 */),
    Log(String),
    Finished(Result<(), String>),
}

struct FpgaProgress {
    tx: Sender<TaskMessage>,
    current_max: u64,
    op: String,
}

impl lib::FpgaProgressCallbacks for FpgaProgress {
    fn init(&mut self, message: &str, max_progress: u64) {
        self.current_max = max_progress;
        self.op = message.to_string();
        let _ = self.tx.send(TaskMessage::Log(format!("[FPGA] {}", message)));
        let _ = self.tx.send(TaskMessage::Progress(format!("[FPGA] {}: {}/{}", message, 0, max_progress), 0.0));
    }

    fn update(&mut self, progress: u64) {
        if self.current_max > 0 {
            let frac = (progress as f32 / self.current_max as f32).clamp(0.0, 1.0);
            let _ = self.tx.send(TaskMessage::Progress(format!("[FPGA] {}: {}/{}", self.op, progress, self.current_max), frac));
        }
    }

    fn message(&mut self, message: &str) {
        let _ = self.tx.send(TaskMessage::Log(format!("[FPGA] {}", message)));
    }

    fn error(&mut self, message: &str) {
        let _ = self.tx.send(TaskMessage::Log(format!("[FPGA] ERROR: {}", message)));
    }

    fn success(&mut self) {
        let _ = self.tx.send(TaskMessage::Log("[FPGA] Update complete.".to_string()));
    }
}

struct McuProgress {
    tx: Sender<TaskMessage>,
    total_len: usize,
}

impl espflash::target::ProgressCallbacks for McuProgress {
    fn init(&mut self, addr: u32, len: usize) {
        self.total_len = len;
        let _ = self.tx.send(TaskMessage::Log(format!("[MCU] Initializing flash at {:#X}, size: {} bytes", addr, len)));
    }

    fn update(&mut self, current: usize) {
        if self.total_len > 0 {
            let frac = (current as f32 / self.total_len as f32).clamp(0.0, 1.0);
            let _ = self.tx.send(TaskMessage::Progress(format!("[MCU] Write Flash: {}/{} bytes", current, self.total_len), frac));
        }
    }

    fn verifying(&mut self) {
        let _ = self.tx.send(TaskMessage::Log("[MCU] Verifying flash...".to_string()));
    }

    fn finish(&mut self, _skipped: bool) {
        let _ = self.tx.send(TaskMessage::Log("[MCU] Flash finished.".to_string()));
    }
}

struct GuiApp {
    firmwares: Vec<Firmware>,
    selected_firmware_idx: usize,
    selected_version_idx: usize,
    mode: Mode,

    // Background task communication
    task_rx: Option<Receiver<TaskMessage>>,
    task_tx: Sender<TaskMessage>,
    is_running: bool,
    current_status: String,
    current_progress: f32,
    logs: Vec<String>,
}

impl GuiApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        GuiApp::default()
    }
}

impl Default for GuiApp {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        let firmware_options = get_firmware_options();
        Self {
            firmwares: firmware_options,
            selected_firmware_idx: 0,
            selected_version_idx: 0,
            mode: Mode::WriteFlash,
            task_rx: Some(rx),
            task_tx: tx,
            is_running: false,
            current_status: "Ready".to_string(),
            current_progress: 0.0,
            logs: vec![],
        }
    }
}

impl eframe::App for GuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.task_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    TaskMessage::Progress(status, prog) => {
                        self.current_status = status;
                        self.current_progress = prog.clamp(0.0, 1.0);
                    }
                    TaskMessage::Log(line) => {
                        self.logs.push(line);
                    }
                    TaskMessage::Finished(res) => {
                        self.is_running = false;
                        match res {
                            Ok(()) => {
                                self.current_status = "Firmware change completed!".to_string();
                                self.current_progress = 1.0;
                                self.logs.push("[SUCCESS] Unplug and turn off your Chromatic, take out the batteries, wait a few minutes, then turn it on again.".to_string());
                            }
                            Err(e) => {
                                self.current_status = format!("Error: {}", e);
                                self.current_progress = 0.0;
                                self.logs.push(format!("[ERROR] {}", e));
                            }
                        }
                    }
                }
            }
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                match count_chromatics() {
                    Ok(counts) => {
                        if counts.chromatic == 1 && counts.gwu2x == 1 {
                            ui.colored_label(egui::Color32::GREEN, "✔ Connected: 1 Chromatic & 1 GoWin FPGA interface found.");
                        } else {
                            ui.colored_label(
                                egui::Color32::YELLOW,
                                format!("⚠ Warning: Found {} Chromatics, {} GoWin interfaces (Expected 1 of each).", counts.chromatic, counts.gwu2x)
                            );
                        }
                    }
                    Err(e) => {
                        ui.colored_label(egui::Color32::RED, format!("✖ USB Device enumeration error: {}", e));
                    }
                }
            });

            ui.add_space(10.0);

            if !self.firmwares.is_empty() {
                ui.label(egui::RichText::new("Firmware:").strong());
                for (idx, fw) in self.firmwares.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.radio_value(&mut self.selected_firmware_idx, idx, &fw.title).clicked() {
                            self.selected_version_idx = 0;
                        }
                    });

                    ui.indent(format!("indent-{}", idx), |ui| {
                        ui.label(&fw.description);
                    });
                }

                ui.horizontal(|ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(egui::RichText::new("Version:").strong());
                    let current_fw = &self.firmwares[self.selected_firmware_idx];

                    if self.selected_version_idx >= current_fw.versions.len() {
                        self.selected_version_idx = 0;
                    }

                    let current_version_str = &current_fw.versions[self.selected_version_idx].version;
                    egui::ComboBox::from_id_salt("version_combo")
                        .selected_text(current_version_str)
                        .show_ui(ui, |ui| {
                            for (idx, ver) in current_fw.versions.iter().enumerate() {
                                ui.selectable_value(&mut self.selected_version_idx, idx, &ver.version);
                            }
                        });
                });
            } else {
                ui.colored_label(egui::Color32::RED, "No firmware options returned from library.");
            }

            ui.add_space(10.0);

            ui.horizontal(|ui| {
                let btn_text = if self.is_running { "Flashing in Progress..." } else { "Flash Firmware" };
                if ui.add_enabled(!self.is_running && !self.firmwares.is_empty(), egui::Button::new(btn_text).min_size(egui::vec2(180.0, 32.0))).clicked() {
                    self.start_flashing_task();
                }
            });

            ui.add_space(10.0);
            ui.label(&self.current_status);

            let progress_bar = egui::ProgressBar::new(self.current_progress)
                .show_percentage()
                .animate(self.is_running);
            ui.add(progress_bar);

            ui.add_space(10.0);
            ui.label(egui::RichText::new("Console Output / Logs").strong());

            let scroll_area = egui::ScrollArea::vertical().max_height(150.0).stick_to_bottom(true);
            scroll_area.show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                for log in &self.logs {
                    ui.label(log);
                }
            });
        });

        // Request continuous repaint while running for animated progress bars
        if self.is_running {
            ui.request_repaint();
        }
    }
}

impl GuiApp {
    fn start_flashing_task(&mut self) {
        self.is_running = true;
        self.current_progress = 0.0;
        self.current_status = "Preparing firmware download and verification...".to_string();
        self.logs.push(format!("--- Starting operation in mode: {:?} ---", self.mode));

        let tx = self.task_tx.clone();
        let fw_option = self.firmwares[self.selected_firmware_idx].clone();
        let version_option = fw_option.versions[self.selected_version_idx].clone();
        let mode = self.mode;

        thread::spawn(move || {
            let _ = tx.send(TaskMessage::Log(format!("Selected Firmware: {} ({})", fw_option.title, version_option.version)));

            // Determine download directory
            let download_dir = lib::download_dir().unwrap();

            let fpga_filename = lib::fpga_local_filename(&fw_option, &version_option);
            let fpga_path = download_dir.join(fpga_filename);

            let mcu_filename = lib::mcu_local_filename(&fw_option, &version_option);
            let mcu_path = download_dir.join(mcu_filename);

            // Ensure FPGA binary is available & verified
            let _ = tx.send(TaskMessage::Progress("Verifying / Downloading FPGA binary...".to_string(), 0.15));
            match ensure_available(&version_option.fpga_url, &version_option.fpga_sha256, &fpga_path, &format!("{} - FPGA", fw_option.title), &tx) {
                Ok(true) => { let _ = tx.send(TaskMessage::Log("FPGA file hash verified.".to_string())); }
                Ok(false) => {
                    let _ = tx.send(TaskMessage::Finished(Err("FPGA file hash verification failed (Hash mismatch).".to_string())));
                    return;
                }
                Err(e) => {
                    let _ = tx.send(TaskMessage::Finished(Err(format!("Failed to fetch FPGA binary: {}", e))));
                    return;
                }
            }

            // Ensure MCU binary is available & verified
            let _ = tx.send(TaskMessage::Progress("Verifying / Downloading MCU binary...".to_string(), 0.35));
            match ensure_available(&version_option.mcu_url, &version_option.mcu_sha256, &mcu_path, &format!("{} - MCU", fw_option.title), &tx) {
                Ok(true) => { let _ = tx.send(TaskMessage::Log("MCU file hash verified.".to_string())); }
                Ok(false) => {
                    let _ = tx.send(TaskMessage::Finished(Err("MCU file verification failed (Hash mismatch).".to_string())));
                    return;
                }
                Err(e) => {
                    let _ = tx.send(TaskMessage::Finished(Err(format!("Failed to fetch MCU binary: {}", e))));
                    return;
                }
            }

            if mode == Mode::DownloadOnly {
                let _ = tx.send(TaskMessage::Log(format!("Downloaded FPGA to {}", fpga_path.display())));
                let _ = tx.send(TaskMessage::Log(format!("Downloaded MCU to {}", mcu_path.display())));
                let _ = tx.send(TaskMessage::Finished(Ok(())));
                return;
            }

            // Target mapping
            let target = match mode {
                Mode::WriteFlash => Target::Flash,
                Mode::WriteSRAM => Target::SRAM,
                Mode::DownloadOnly => unreachable!(),
            };

            let _ = tx.send(TaskMessage::Progress("Programming FPGA hardware...".to_string(), 0.60));
            let mut fpga_progress = FpgaProgress { tx: tx.clone(), current_max: 0, op: "".to_string() };
            if let Err(e) = flash_fpga(&fpga_path, target, Some(&mut fpga_progress)) {
                let _ = tx.send(TaskMessage::Finished(Err(format!("FPGA flashing failed: {}", e))));
                return;
            }
            let _ = tx.send(TaskMessage::Log("FPGA programming completed.".to_string()));

            // Flash MCU if target is Flash
            if target == Target::Flash {
                let _ = tx.send(TaskMessage::Progress("Programming MCU Flash...".to_string(), 0.85));
                let mut mcu_progress = McuProgress { tx: tx.clone(), total_len: 0 };
                if let Err(e) = flash_mcu(&mcu_path, &mut mcu_progress) {
                    let _ = tx.send(TaskMessage::Finished(Err(format!("MCU flashing failed: {}", e))));
                    return;
                }
                let _ = tx.send(TaskMessage::Log("MCU update complete.".to_string()));
            } else {
                let _ = tx.send(TaskMessage::Log("Skipping MCU write (WriteSRAM mode selected).".to_string()));
            }

            let _ = tx.send(TaskMessage::Finished(Ok(())));
        });
    }
}

fn ensure_available(url_str: &str, expected_hash: &str, path: &PathBuf, title: &str, tx: &Sender<TaskMessage>) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    if path.exists() {
        match verify_hash(path, expected_hash) {
            Ok(HashOutcome::Match) => return Ok(true),
            Ok(HashOutcome::Mismatch { .. }) => {
                let _ = tx.send(TaskMessage::Log(format!("Incorrect hash for {}, removing existing file...", path.display())));
                let _ = fs::remove_file(path);
            }
            Err(e) => {
                let _ = tx.send(TaskMessage::Log(format!("Hash verification warning: {}, re-downloading...", e)));
                let _ = fs::remove_file(path);
            }
        }
    }

    let _ = tx.send(TaskMessage::Log(format!("Fetching {} from {}...", title, url_str)));

    let url = Url::parse(url_str)?;
    if url.scheme() == "file" {
        let local_path = url.to_file_path().map_err(|_| "Failed to convert file:// URL")?;
        if !local_path.exists() {
            return Err(format!("Local file not found: {}", local_path.display()).into());
        }
        fs::copy(&local_path, path)?;
    } else {
        let mut response = reqwest::blocking::get(url_str)?;
        let mut file = fs::File::create(path)?;
        std::io::copy(&mut response, &mut file)?;
    }

    match verify_hash(path, expected_hash)? {
        HashOutcome::Match => Ok(true),
        HashOutcome::Mismatch { .. } => Ok(false),
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([580.0, 500.0])
            .with_min_inner_size([480.0, 420.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Chromatic EZ Firmware",
        options,
        Box::new(|cc| Ok(Box::new(GuiApp::new(cc)))),
    )
}