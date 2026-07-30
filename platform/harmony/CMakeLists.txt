cmake_minimum_required(VERSION 3.16)
project(rish_napi LANGUAGES CXX)

add_library(rish_core SHARED IMPORTED)
set_target_properties(
  rish_core
  PROPERTIES IMPORTED_LOCATION
             "${CMAKE_CURRENT_LIST_DIR}/libs/${OHOS_ARCH}/librish_ffi.so"
)

add_library(rish_ffi SHARED rish_napi.cpp)
target_compile_features(rish_ffi PRIVATE cxx_std_17)
target_link_libraries(rish_ffi PRIVATE rish_core libace_napi.z.so)
