cmake_minimum_required(VERSION 3.22.1)
project(rish_jni LANGUAGES CXX)

add_library(rish_ffi SHARED IMPORTED)
set_target_properties(
  rish_ffi
  PROPERTIES
    IMPORTED_LOCATION
      "${CMAKE_CURRENT_LIST_DIR}/jniLibs/${ANDROID_ABI}/librish_ffi.so"
    # Rust cdylibs do not carry an ELF SONAME by default. Without this,
    # CMake can record the build-machine absolute path in DT_NEEDED.
    IMPORTED_NO_SONAME TRUE
)

add_library(rish_jni SHARED rish_jni.cpp)
target_compile_features(rish_jni PRIVATE cxx_std_17)
target_link_libraries(rish_jni PRIVATE rish_ffi)
