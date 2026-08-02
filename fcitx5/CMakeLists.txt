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
project(fcitx5-ttyskk VERSION 0.2.1 LANGUAGES CXX)

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
#
# **毎回 cargo を呼ぶ。** add_custom_command(OUTPUT ...) だと、出来上がりが既にある
# 限り再実行されない (DEPENDS にソースを並べない限り)。capi を直したのに古い
# libttyskk.a がリンクされ、**実行時に undefined symbol で落ちる**。cargo は自分で
# 差分を見るので、毎回呼んでも無駄にならない。
add_custom_target(ttyskk-capi ALL
  COMMAND ${CARGO_EXECUTABLE} build --release -p ttyskk-capi
  WORKING_DIRECTORY ${CARGO_ROOT}
  BYPRODUCTS ${CAPI_LIB} ${CAPI_HEADER}
  COMMENT "変換エンジン (Rust) を組み立てる"
  VERBATIM)

# ---- addon ----
#
# 出来上がりは libttyskk.so。**fcitx5 は Library= の値をそのままファイル名として探す**
# ので、addon の登録 (ttyskk-addon.conf) は Library=libttyskk と書く必要がある。

add_library(ttyskk MODULE ttyskk.cpp)
add_dependencies(ttyskk ttyskk-capi)
target_include_directories(ttyskk PRIVATE "${CARGO_ROOT}/capi")
target_link_libraries(ttyskk PRIVATE Fcitx5::Core ${CAPI_LIB})
# Rust の標準ライブラリが要求するもの
target_link_libraries(ttyskk PRIVATE pthread dl m)
# 呼べない関数があればリンクの時点で止める。実行時の undefined symbol まで
# 持ち越すと、fcitx5 が丸ごと落ちて原因が見えにくい。
target_link_options(ttyskk PRIVATE -Wl,--no-undefined)
set_target_properties(ttyskk PROPERTIES PREFIX "lib")

install(TARGETS ttyskk DESTINATION "${FCITX_INSTALL_LIBDIR}/fcitx5")
install(FILES ttyskk-addon.conf
        RENAME ttyskk.conf
        DESTINATION "${FCITX_INSTALL_PKGDATADIR}/addon")
install(FILES ttyskk.conf
        DESTINATION "${FCITX_INSTALL_PKGDATADIR}/inputmethod")

# ---- アイコン ----
#
# **モードごとに別の絵を置く。** 札 (トレイ) の絵が変われば、いま かな なのか カナ なのか
# を見るために視線を動かすだけで済む。切り替えの吹き出しは消えてしまうので、常時見える
# 手掛かりはこちらだけ。名前は engine.cpp の subModeIconImpl と揃っている必要がある。
#
# SVG と PNG の両方を置く。**環境によっては札が SVG を描けない**ので、既存の入力メソッド
# に倣う。中身は fcitx5/icons/generate.py が作る (普段は走らせない。出来上がりを置く)。

set(TTYSKK_ICONS
  fcitx-ttyskk
  fcitx-ttyskk-hiragana
  fcitx-ttyskk-katakana
  fcitx-ttyskk-halfkana
  fcitx-ttyskk-fullwidth
  fcitx-ttyskk-latin)
set(TTYSKK_ICON_SIZES 16 22 24 32 48 64)

foreach(icon ${TTYSKK_ICONS})
  install(FILES "icons/${icon}.svg"
          DESTINATION "${FCITX_INSTALL_DATADIR}/icons/hicolor/scalable/apps")
  foreach(px ${TTYSKK_ICON_SIZES})
    install(FILES "icons/${icon}-${px}.png"
            RENAME "${icon}.png"
            DESTINATION "${FCITX_INSTALL_DATADIR}/icons/hicolor/${px}x${px}/apps")
  endforeach()
endforeach()
