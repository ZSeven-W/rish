cmake_minimum_required(VERSION 3.16)
project(rish_napi LANGUAGES CXX)

add_library(rish_ffi SHARED IMPORTED)
set_target_properties(
  rish_ffi
  PROPERTIES
    IMPORTED_LOCATION
      "${CMAKE_CURRENT_LIST_DIR}/libs/${OHOS_ARCH}/librish_ffi.so"
    # Keep the device dependency relocatable when the Rust cdylib has no
    # ELF SONAME instead of embedding its build-machine absolute path.
    IMPORTED_NO_SONAME TRUE
)

add_library(rish_napi SHARED rish_napi.cpp)
target_compile_features(rish_napi PRIVATE cxx_std_17)
target_link_libraries(rish_napi PRIVATE rish_ffi libace_napi.z.so)
