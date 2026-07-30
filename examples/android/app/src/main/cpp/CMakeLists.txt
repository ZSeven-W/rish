cmake_minimum_required(VERSION 3.22.1)
project(rish_android_demo LANGUAGES CXX)

get_filename_component(
  RISH_REPOSITORY_ROOT
  "${CMAKE_CURRENT_LIST_DIR}/../../../../../.."
  ABSOLUTE
)

add_library(rish_ffi SHARED IMPORTED)
set_target_properties(
  rish_ffi
  PROPERTIES
    IMPORTED_LOCATION
      "${CMAKE_CURRENT_LIST_DIR}/../jniLibs/${ANDROID_ABI}/librish_ffi.so"
    IMPORTED_NO_SONAME TRUE
)

add_library(
  rish_jni
  SHARED
  "${RISH_REPOSITORY_ROOT}/platform/android/rish_jni.cpp"
)
target_compile_features(rish_jni PRIVATE cxx_std_17)
target_compile_options(rish_jni PRIVATE -Wall -Wextra -Werror)
target_link_libraries(rish_jni PRIVATE rish_ffi)
