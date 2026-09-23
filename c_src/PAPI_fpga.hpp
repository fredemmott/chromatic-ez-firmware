// Copyright 2026 Fred Emmott <fred@fredemmott.com>
// SPDX-License-Identifier: MIT
#pragma once
#include <stdint.h>
#include <stddef.h>

#ifdef _WIN32
#define LK_CHROMATIC_EXPORT __declspec(dllexport)
#else
#define LK_CHROMATIC_EXPORT
#endif

extern "C" {

using PAPIStringCallback = void (*)(const char*, uint16_t);
using PAPIProgressCallback = void(*)(size_t value, size_t max);

// 1 on success, 0 on failure
LK_CHROMATIC_EXPORT int papi_fpga_program_sram(
  const char* path,
  size_t path_len,
  PAPIStringCallback message_callback,
  PAPIStringCallback error_callback,
  PAPIProgressCallback progress_callback);
// 1 on success, 0 on failure
LK_CHROMATIC_EXPORT int papi_fpga_reset();

}