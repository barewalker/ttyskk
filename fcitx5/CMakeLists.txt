# ttyskk を fcitx5 の入力メソッドとして組み立てる。
#
# 変換そのものは Rust 側 (capi/) が持っている。**静的に取り込む**ので、出来上がりは
# libttyskk.so 一つで完結する。共有ライブラリにすると Rust 側も libttyskk.so になって
# addon と名前がぶつかるうえ、置き場所を探させることになる。
#
#   cmake -B build -DCMAKE_INSTALL_PREFIX=/usr
#   cmake --build build
#   sudo cmake --install build

cmake_minimum_required(VERSION 3.16)
project(fcitx5-ttyskk VERSION 0.1.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

find_package(ECM REQUIRED)
set(CMAKE_MODULE_PATH ${ECM_MODULE_PATH} ${CMAKE_MODULE_PATH})
find_package(Fcitx5Core REQUIRED)

# ---- Rust 側 ----

set(CARGO_ROOT "${CMAKE_CURRENT_SOURCE_DIR}/..")
set(CARGO_TARGET_DIR "${CARGO_ROOT}/target")
set(CAPI_LIB "${CARGO_TARGET_DIR}/release/libttyskk.a")
set(CAPI_HEADER "${CARGO_ROOT}/capi/ttyskk.h")

find_program(CARGO_EXECUTABLE cargo REQUIRED)

# ヘッダは cargo が (build.rs から cbindgen で) 一緒に作る。
add_custom_command(
  OUTPUT ${CAPI_LIB} ${CAPI_HEADER}
  COMMAND ${CARGO_EXECUTABLE} build --release -p ttyskk-capi
  WORKING_DIRECTORY ${CARGO_ROOT}
  COMMENT "変換エンジン (Rust) を組み立てる"
  VERBATIM)
add_custom_target(ttyskk-capi DEPENDS ${CAPI_LIB} ${CAPI_HEADER})

# ---- addon ----

add_library(ttyskk MODULE ttyskk.cpp)
add_dependencies(ttyskk ttyskk-capi)
target_include_directories(ttyskk PRIVATE "${CARGO_ROOT}/capi")
target_link_libraries(ttyskk PRIVATE Fcitx5::Core ${CAPI_LIB})
# Rust の標準ライブラリが要求するもの
target_link_libraries(ttyskk PRIVATE pthread dl m)
set_target_properties(ttyskk PROPERTIES PREFIX "lib")

install(TARGETS ttyskk DESTINATION "${FCITX_INSTALL_LIBDIR}/fcitx5")
install(FILES ttyskk-addon.conf
        RENAME ttyskk.conf
        DESTINATION "${FCITX_INSTALL_PKGDATADIR}/addon")
install(FILES ttyskk.conf
        DESTINATION "${FCITX_INSTALL_PKGDATADIR}/inputmethod")
