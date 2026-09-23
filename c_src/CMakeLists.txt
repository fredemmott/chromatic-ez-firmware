cmake_minimum_required(VERSION 3.20)
project(openFPGAloader-chromatic C CXX)

include(FetchContent)
include(GNUInstallDirs)

if (LINUX)
  add_compile_options(-fPIC)
endif()

if (MSVC)
    add_compile_options(
        # Enable conformant mode for the C preprocessor
        "/Zc:preprocessor"
        # C++ standard exception handling
        "/EHsc"
        # UTF-8 sources
        "/utf-8"
    )
endif()

set(LIBUSB_INSTALL_TARGETS OFF)
FetchContent_Declare(libusb_cmake
  GIT_REPOSITORY https://github.com/libusb/libusb-cmake.git
  GIT_TAG        c8477c10ac2ac6b1718d4d498e102b9f18b776f5 # v1.0.30-0
)
FetchContent_MakeAvailable(libusb_cmake)

add_subdirectory(openFPGAloader)

add_library(
  openFPGAloader-chromatic
  STATIC
  PAPI_fpga.cpp
  PAPI_fpga.hpp
)

set_target_properties(
  openFPGAloader-chromatic
  PROPERTIES
  C_STANDARD 11
  C_STANDARD_REQUIRED ON
  CXX_STANDARD 23
  CXX_STANDARD_REQUIRED ON
  CXX_EXTENSIONS OFF
)

target_link_libraries(
  openFPGAloader-chromatic
  PRIVATE
  openFPGAloader
  usb-1.0 # In future versions, this will be libusb::usb-1.0
)

install(
    TARGETS
    openFPGAloader
    openFPGAloader-chromatic
    usb-1.0
    ARCHIVE DESTINATION ${CMAKE_INSTALL_LIBDIR}
    LIBRARY DESTINATION ${CMAKE_INSTALL_LIBDIR}
    RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR}
)

if (WIN32)
    target_compile_definitions(
      openFPGAloader-chromatic
      PRIVATE
      WIN32_LEAN_AND_MEAN
      NOMINMAX
    )
endif()
