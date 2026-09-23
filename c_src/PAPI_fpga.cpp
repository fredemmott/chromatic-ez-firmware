// Copyright 2026 Fred Emmott <fred@fredemmott.com>
// SPDX-License-Identifier: MIT
#include "PAPI_fpga.hpp"

#include "openFPGAloader/jtag.hpp"
#include "openFPGAloader/gowin.hpp"
#include "openFPGAloader/progressBar.hpp"

#include <format>
#include <functional>
#include <utility>
#include <print>

namespace {

template<std::invocable<Gowin&> T>
void fpga_invoke(T&& fn, const std::string& path, const Device::prog_type_t prog_type) {
  const auto& cable = cable_list.at("gwu2x");
  jtag_pins_conf_t pins_config {};
  Jtag jtag {
    cable,
    &pins_config,
    /* args.device = */ {},
    /* args.ftdi_serial = */ {},
    /* args.freq = */ 6'000'000, // DEFAULT_FREQ from main.cpp
    /* args.verbose = */ 0,
    /* args.ip_adr = */ "127.0.0.1",
    /* args.port = */ 0,
  };
  Gowin fpga {
    &jtag,
    path,
    /* args.file_type = */ {},
    /* args.mcufw = */ {},
    prog_type,
    /* args.external_flash = */ false,
    /* args.verify = */ false,
    /* args.verbose = */ 0,
    /* args.user_flash = */ {},
  };
  std::invoke(std::forward<T>(fn), fpga);
}

PAPIStringCallback gErrorCallback { nullptr };
PAPIStringCallback gMessageCallback { nullptr };

template<class... Args>
void message(
    const std::format_string<Args...>& fmt,
    Args&&... args) {
  if (!gMessageCallback) return;
  const auto msg = std::vformat(fmt.get(), std::make_format_args(args...));
  gMessageCallback(msg.data(), static_cast<uint16_t>(msg.size()));
};

template<class... Args>
void error_message(
    const std::format_string<Args...>& fmt,
    Args&&... args) {
  if (!gErrorCallback) return;
  const auto msg = std::vformat(fmt.get(), std::make_format_args(args...));
  gErrorCallback(msg.data(), static_cast<uint16_t>(msg.size()));
};

PAPIProgressCallback gProgressCallback { nullptr };
PAPIProgressResetCallback gProgressResetCallback {nullptr };
std::size_t gProgressMax {};

enum class Target {
    Flash,
    SRAM,
};

int fpga_program(
  const Target target,
  const char* const path,
  const size_t path_len,
  const PAPIStringCallback message_callback,
  const PAPIStringCallback error_callback,
  const PAPIProgressResetCallback progress_reset_callback,
  const PAPIProgressCallback progress_callback) {

try {
  gMessageCallback = message_callback;
  gErrorCallback = error_callback;
  gProgressResetCallback = progress_reset_callback;
  gProgressCallback = progress_callback;
  const struct CallbackGuard {
    ~CallbackGuard() {
      gMessageCallback = nullptr;
      gErrorCallback = nullptr;
      gProgressResetCallback = nullptr;
      gProgressCallback = nullptr;
    }
  } callbackGuard;

  message("Connecting...");

  fpga_invoke(
    [=](Gowin& fpga) {
      fpga.program(/* offset = */ 0, /* unprotect_flash = */ 0);
    },
    {path, path_len},
    (target == Target::Flash) ? Device::prog_type_t::WR_FLASH : Device::prog_type_t::WR_SRAM
  );

  message("Rebooting...");
  return 1;
} catch (const std::exception& e) {
  error_message("uncaught exception in papi_fpga_program_sram(): {}", e.what());
  return 0;
}
}

}

extern "C" int papi_fpga_program_sram(
  const char* const path,
  const size_t path_len,
  const PAPIStringCallback message_callback,
  const PAPIStringCallback error_callback,
  const PAPIProgressResetCallback progress_reset_callback,
  const PAPIProgressCallback progress_callback) {
  return fpga_program(Target::SRAM, path, path_len, message_callback, error_callback, progress_reset_callback, progress_callback);
}

extern "C" int papi_fpga_program_flash(
  const char* const path,
  const size_t path_len,
  const PAPIStringCallback message_callback,
  const PAPIStringCallback error_callback,
  const PAPIProgressResetCallback progress_reset_callback,
  const PAPIProgressCallback progress_callback) {
  return fpga_program(Target::Flash, path, path_len, message_callback, error_callback, progress_reset_callback, progress_callback);
}

int papi_fpga_reset() try {
  fpga_invoke(&Gowin::reset, {}, Device::prog_type_t::PRG_NONE);
  return 1;
} catch (const std::exception& e) {
  error_message("uncaught exception in papi_fpga_reset(): {}", e.what());
  return 0;
}

// openFPGAloader stubs

void printError(const std::string &err, bool eol) {
  error_message("openFPGAloader ERROR: {}", err);
}
void printWarn(const std::string &warn, bool eol) {
  error_message("openFPGAloader WARNING: {}", warn);
}
void printInfo(const std::string &info, bool eol) {
}
void printSuccess(const std::string &success, bool eol) {
}

ProgressBar::ProgressBar(const std::string &mess, int maxValue, int progressLen,
                         bool quiet) {
  gProgressMax = maxValue;
  if (gProgressResetCallback) {
    gProgressResetCallback(mess.data(), mess.size(), maxValue);
  }
}
void ProgressBar::display(int value, char force) {
  if (gProgressCallback) {
    gProgressCallback(value);
  }
}
void ProgressBar::done() {
  gProgressCallback(std::exchange(gProgressMax, 0));
}

void ProgressBar::fail() {
  done(); // result code of operation is used
}
