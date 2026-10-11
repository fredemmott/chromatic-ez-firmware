use std::path::Path;
use crate::{count_chromatics, Target, HashOutcome};
use indicatif::{ProgressBar, ProgressStyle};

pub mod colors {
    pub const COLOR_RESET: &str = "\x1b[0m";
    pub const COLOR_RED: &str = "\x1b[31;1m";
    pub const COLOR_GREEN: &str = "\x1b[32;1m";
    pub const COLOR_YELLOW: &str = "\x1b[33;1m";
    pub const COLOR_CYAN: &str = "\x1b[36;1m";
    pub const COLOR_BOLD: &str = "\x1b[1m";
    pub const COLOR_DIM: &str = "\x1b[2m";
}

pub fn linkify(url: impl std::fmt::Display, label: impl std::fmt::Display) -> String {
    format!("\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\", url, label)
}

pub fn have_single_chromatic_target() -> anyhow::Result<bool> {
    use colors::*;

    let counts = count_chromatics()?;

    if counts.chromatic == 0 {
       println!("{COLOR_RED}No Chromatics were found via USB. Is your console plugged in?{COLOR_RESET}");
        return Ok(false);
    }
    if counts.chromatic != 1 {
        println!("{COLOR_RED}Found {} Chromatic devices, required exactly 1.{COLOR_RESET}", counts.chromatic);
        return Ok(false);
    }


    if counts.gwu2x != 1 {
        println!("{COLOR_RED}Found {} GoWin FPGA interfaces, required exactly 1.{COLOR_RESET}", counts.gwu2x);
        return Ok(false);
    }
    println!("{COLOR_GREEN}Found 1 Chromatic device and 1 GoWin FPGA interface.{COLOR_RESET}");
    Ok(true)
}

pub struct McuProgress {
    pb: Option<ProgressBar>,
}

impl Default for McuProgress {
    fn default() -> Self {
        Self { pb: None }
    }
}

pub struct FpgaProgress {
    progress_bar: Option<ProgressBar>,
}

impl Default for FpgaProgress {
    fn default() -> Self {
        Self { progress_bar: None }
    }
}

impl crate::FpgaProgressCallbacks for FpgaProgress {
    fn init(&mut self, message: &str, max_progress: u64) {
        let style = ProgressStyle::default_bar()
            .template("{msg:.cyan} [{elapsed_precise}] [{wide_bar:.cyan/blue}] ({eta} remaining)")
            .unwrap()
            .progress_chars("##-");
        let pb = ProgressBar::new(max_progress)
            .with_style(style)
            .with_message(message.to_string());
        self.progress_bar = Some(pb);
    }

    fn update(&mut self, progress: u64) {
        if let Some(pb) = &mut self.progress_bar {
            pb.set_position(progress);
        }
    }

    fn message(&mut self, message: &str) {
        println!("{message}");
    }

    fn error(&mut self, message: &str) {
        use colors::*;
        println!("{COLOR_RED}{message}{COLOR_RESET}");
    }

    fn success(&mut self) {
        use colors::*;
        println!("\n{COLOR_GREEN}FPGA update complete.{COLOR_RESET}");
    }
}

impl espflash::target::ProgressCallbacks for McuProgress {
    fn init(&mut self, addr: u32, len: usize) {
        let style = ProgressStyle::default_bar()
            .template("{msg:.cyan} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta} remaining)")
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("##-");
        let pb = ProgressBar::new(len as u64)
            .with_style(style)
            .with_message(format!("Flashing MCU at {addr:#X}"));
        self.pb = Some(pb);
    }

    fn update(&mut self, current: usize) {
        if let Some(ref pb) = self.pb {
            pb.set_position(current as u64);
        }
    }

    fn verifying(&mut self) {
        if let Some(ref pb) = self.pb {
            pb.set_message("Verifying...");
        }
    }

    fn finish(&mut self, _skipped: bool) {
        if let Some(ref pb) = self.pb {
            pb.finish_and_clear();
        }
    }
}

pub fn flash_fpga(fpga_path: &Path, target: Target) -> anyhow::Result<()> {
    use colors::*;
    println!("\n{COLOR_GREEN}Programming FPGA...{COLOR_RESET}\n");

    let ret = crate::flash_fpga(fpga_path, target, Some(&mut FpgaProgress::default()));
    match ret {
        Ok(()) => Ok(()),
        Err(e) => {
            println!("\n{COLOR_RED}openFPGALoader failed with status: {e}{COLOR_RESET}");
            Err(e)
        }
    }
}

pub fn flash_mcu(mcu_path: &Path) -> anyhow::Result<()> {
    use colors::*;

    println!("\n{COLOR_GREEN}Programming MCU...{COLOR_RESET}\n");
    match crate::flash_mcu(mcu_path) {
        Ok(()) => {
            println!("\n{COLOR_GREEN}MCU update complete.{COLOR_RESET}");
            Ok(())
        },
        Err(e) => {
            println!("\n{COLOR_RED}MCU update failed: {e}{COLOR_RESET}");
            Err(e)
        }
    }
}

pub fn verify_hash(file_path: &Path, expected: &str) -> anyhow::Result<bool> {
    use colors::*;

    let file_name = file_path.file_name().unwrap().to_string_lossy();
    println!("{COLOR_CYAN}Verifying hash for {file_name}...{COLOR_RESET}");

    match crate::verify_hash(file_path, expected)? {
        HashOutcome::Match => {
            println!("{COLOR_GREEN}Verification OK.{COLOR_RESET}");
            Ok(true)
        },
        HashOutcome::Mismatch { actual} => {
            println!("{COLOR_RED}Verification FAILED!{COLOR_RESET}");
            println!("  Expected: {expected}");
            println!("  Actual:   {actual}");
            Ok(false)
        }
    }
}