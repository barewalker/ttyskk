# fcitx5 の addon にする — 設計

GUI でも ttyskk の変換エンジンを使い、**辞書と学習を端末と共有する**ための設計。
実装はまだ無い。ここは着手前の見取り図。

## なぜ fcitx5 か

GNOME + Wayland では選択肢が実質ここしかない。Wayland には `input-method-v2` という
基盤に依存せず入力メソッドを書くためのプロトコルがあるが、**GNOME (Mutter) は実装して
いない** (KDE の KWin、sway、Hyprland は対応)。GNOME を使う限り、独立した入力メソッド
の道は塞がれている。IBus に載せる手もあるが、日本語入力で広く使われているのは fcitx5 で、
addon の書きやすさでも優る。

同じ道を通った先例に [cskk](https://github.com/naokiri/cskk) と
[fcitx5-cskk](https://github.com/fcitx/fcitx5-cskk) がある (どちらも GPL-3.0)。**構造の
参考にはするが、コードは写さない** — ttyskk は MIT OR Apache-2.0 で、混ぜるとライセンス
が変わってしまう。addon の書き方を写経するなら fcitx5 本体 (LGPL-2.1+) の方が安全。

## 三つの層

```
┌─────────────────────────────────────────┐
│ fcitx5-ttyskk  (C++)                    │  fcitx5 の InputMethodEngine
│  キーイベント → 変換 → preedit/候補/確定 │  CMake でビルド
├─────────────────────────────────────────┤
│ ttyskk-capi    (Rust, cdylib)           │  C ABI。cbindgen でヘッダ生成
├─────────────────────────────────────────┤
│ ttyskk         (Rust, lib)              │  変換エンジン (実装済み)
└─────────────────────────────────────────┘
```

lib は `default-features = false` で使う。端末側の依存 (擬似端末・端末制御) は落ちる。

## lib 側に要る変更

土台はできている (`Skk::handle` / `preedit` / `candidates`、`Response` の
`commit` と `passthrough` の分離)。残るのは次の三つ。

### 1. 入力コンテキストごとの状態をどう持つか

fcitx5 は窓ごとに `InputContext` を作り、状態は `InputContextProperty` に持たせる作法。
素直に従うと **`Skk` を IC ごとに作る**ことになるが、`Skk` は `Dict` を所有していて、
共有辞書は 17 万語ある。IC の数だけ複製はできない。

| 案 | 中身 | 評価 |
|---|---|---|
| **A. Skk は一つ** | IC が切り替わったら確定して `reset` | **まずこれ**。lib に変更が要らない。窓を離れると preedit が確定するのは IME として自然な挙動 |
| B. Dict を共有 | `Arc<Mutex<Dict>>` にして `Skk` を IC ごとに | fcitx5 の作法としては正しい。lib の API 変更が要る。学習が窓をまたいで即座に反映される |

**A で始めて、必要になったら B へ移る。** A の弱点は「窓 X で ▽ の途中に窓 Y へ移ると
確定される」だけで、実害が小さい。B に移るときも C++ 側の持ち方が変わるだけで、
C ABI の形は変わらない (ハンドルを一つ持つか複数持つかの違い)。

### 2. preedit からモードの印を外す

`Skk::preedit` が返す `Segment` には `ModeHiragana` / `ModeKatakana` 等が混ざる。
これは端末でカーソルに色を敷くための情報で、**GUI では要らない** (fcitx5 が入力メソッドの
インジケータを自前で出す)。`Preedit::cursor_tint` も同じ。

GUI 向けに、モードの印を含まない形で取り出す口が要る。既存の `preedit()` を変えず、
`Skk::mode()` (実装済み) と組み合わせて C++ 側で捨てるのでも足りる。

### 3. 学習をいつ書き出すか

端末では終了時に一度 `Dict::save()` を呼んでいる。fcitx5 は**長時間動き続ける**ので
同じ手が使えない。`save()` は「ディスクを読み直して、この起動で覚えたことを重ねる」
方式なので、呼ぶたびに利用者辞書の読み書きが走る。打鍵ごとには重い。

- 候補を確定してから数秒〜数十秒の間が空いたら書く (タイマー)
- fcitx5 の停止時 (addon の deinit) にも書く
- 端末側の ttyskk が同時に動いていても、重ねる方式なので消し合わない

## C ABI の形 (実装済み — `capi/`)

以下は当初の見取り図で、**実際に作ったものもほぼこの形**。違いは二点だけ。

- `ttyskk_flush` は要らなかった。**畳めないキーを渡したときに内部で確定する**ように
  したので、呼ぶ側は `ttyskk_key` が false を返したら `ttyskk_commit` を見るだけでよい
- ハンドルの型名は `TtyskkEngine`

生成したヘッダは `capi/ttyskk.h` (cbindgen が `capi/src/lib.rs` から作る。手で直さない)。


```c
typedef struct TtyskkEngine TtyskkEngine;

// 生成・破棄。config は TOML の中身をそのまま渡す (NULL なら既定)
TtyskkEngine *ttyskk_new(const char *system_jisyo_paths,  // ':' 区切り
                         const char *user_jisyo_path,
                         const char *config_toml);
void ttyskk_free(TtyskkEngine *);
void ttyskk_set_config(TtyskkEngine *, const char *config_toml);
void ttyskk_save(TtyskkEngine *);   // 学習の書き出し
void ttyskk_reset(TtyskkEngine *);  // 入力中の内容を捨てる

// キーを一つ渡す。戻り値は「エンジンが処理したか」
//   false なら fcitx5 がそのキーを子アプリへ転送する
bool ttyskk_key(TtyskkEngine *, uint32_t keysym, uint32_t modifiers);

// 直前の ttyskk_key の結果を取り出す。文字列は次の呼び出しまで有効
const char *ttyskk_commit(const TtyskkEngine *);   // 空文字列なら確定なし
int32_t     ttyskk_mode(const TtyskkEngine *);     // 0=ASCII 1=かな …

// preedit。区間ごとに取り出す
size_t      ttyskk_preedit_len(const TtyskkEngine *);
const char *ttyskk_preedit_text(const TtyskkEngine *, size_t i);
int32_t     ttyskk_preedit_style(const TtyskkEngine *, size_t i);

// 候補。▼ でなければ 0
size_t      ttyskk_candidate_len(const TtyskkEngine *);
const char *ttyskk_candidate_text(const TtyskkEngine *, size_t i);
const char *ttyskk_candidate_annotation(const TtyskkEngine *, size_t i);
size_t      ttyskk_candidate_selected(const TtyskkEngine *);
const char *ttyskk_candidate_labels(const TtyskkEngine *);  // 選択キー "asdfjkl"
```

**文字列は Rust 側が保持し、次の呼び出しまで有効**という約束にする。C++ 側で解放しない。
呼び出しごとに確保・解放するより単純で、fcitx5 は同期的に使うので問題にならない。

`keysym` と `modifiers` は X11 の値 (fcitx5 がそのまま持っている)。Rust 側で `Key` に
畳む — この対応表は**エンジンではなく capi の持ち物**にする。端末側の `input.rs` が
バイト列から `Key` を作るのと同じ立場。

| fcitx5 | ttyskk |
|---|---|
| 印字文字 | `Key::Char` |
| Ctrl + 英字 | `Key::Ctrl` |
| Return / BackSpace / Tab / Escape | `Key::Enter` / `Backspace` / `Tab` / `Esc` |
| ISO_Left_Tab (Shift+Tab) | `Key::ShiftTab` |
| 矢印・機能キー | **渡さない** — `ttyskk_key` が false を返して fcitx5 に任せる |

`Key::Raw` は端末専用なので、この経路では作らない。

## C++ 側

```cpp
class TtyskkEngine : public fcitx::InputMethodEngine {
    void keyEvent(const InputMethodEntry &, fcitx::KeyEvent &) override;
    void reset(const InputMethodEntry &, fcitx::InputContextEvent &) override;
    void deactivate(const InputMethodEntry &, fcitx::InputContextEvent &) override;
};
```

`keyEvent` の中でやること。

1. **押下だけ扱う** — `event.isRelease()` なら何もしない
2. `ttyskk_key(keysym, states)` を呼ぶ
3. `ttyskk_commit()` が空でなければ `ic->commitString(...)`
4. preedit を組み立てて `ic->inputPanel().setClientPreedit(...)` → `updatePreedit()`
5. 候補があれば `CommonCandidateList` を作って `inputPanel().setCandidateList(...)`
6. 戻り値が true なら `event.filterAndAccept()`。false なら**何もしない** (fcitx5 が転送)

**確定と転送は同時に起きる。** ▽ の途中で矢印を押すと「見出し語を確定 + 矢印は未処理」に
なるので、`commitString` したうえで accept しない、という組み合わせが要る。fcitx5 では
これができる。`Response` を `commit` と `passthrough` に分けてあるのはこのため。

区間の装飾は次のように対応させる。

| ttyskk の `Style` | fcitx5 |
|---|---|
| `Reading` (▽ と見出し語) | `TextFormatFlag::Underline` |
| `Romaji` (かなになる前) | `TextFormatFlag::Underline` |
| `Candidate` (▼ の候補) | `TextFormatFlag::HighLight` |
| `ListItem` / `ListSelected` | 候補リストへ回すので preedit には入れない |
| `Mode*` | 捨てる (fcitx5 のインジケータが担う) |

モードの表示は fcitx5 の作法に従い、`InputMethodEngine::subMode` か statusArea で
「あ / ア / 半 / Ａ」を出す。ttyskk の `mode_marker` 設定は端末専用なので使わない。

## 設定と辞書

**config.toml を端末と共有する。** `~/.config/ttyskk/config.toml` をそのまま読む。
キーの割り当て・句読点・AZIK・自動変換が端末と GUI で揃うので、覚え直しが要らない。
fcitx5-configtool から設定できないのは不便だが、二重に持って食い違う方が悪い。

**代わりに、歯車を開いたら書き換える先を案内する。** 設定項目を持たないからといって
`Configurable=False` にすると歯車ごと消え、どこを触ればよいのか手掛かりが無くなる。
`ExternalOption` を二つ置き、説明文で場所を伝え、釦で `xdg-open` する形にした。

- **`External` が一つだけだと画面が出ない。** fcitx5-configtool はそれを「画面ではなく
  押した瞬間に起こすもの」と見なして直接コマンドを起こす
  (`ConfigWidget::extractOnlyExternalCommand`)。案内を読ませたいので二つ要る
- **`fcitx://` で始まらない値はコマンド行として実行される** (`launchExternalConfig`)。
  専用の GUI を書かなくても、利用者がふだん使っている編集器を開ける
- **開く相手は先に用意する。** 設定ファイルがまだ無いと `xdg-open` は黙って失敗する
  ので、`getConfig()` の時点で案内だけ書いたものを置く (中身は全部注釈なので動きは
  変わらない)
- **`setConfig()` は読み直しの口に使う。** fcitx5 側に持つ値は無いので、画面から編集器
  を開いて書き換えたあと、そのまま OK で反映される

辞書も同じ場所を使う (`~/.local/share/ttyskk/user.dict`)。これで**端末で覚えた語が
GUI でも先頭に出る**ようになり、当初の動機が満たされる。

fcitx5-skk からの移行では、既に実装済みの取り込み (`~/.local/share/fcitx5/skk/user.dict`
を読む) がそのまま効く。

## ビルドと配置

```
ttyskk/
├── src/            lib + 端末の実行ファイル (現状)
├── capi/           C ABI の層。crate-type = ["cdylib"]
│   └── cbindgen.toml
└── fcitx5/         C++ の addon
    ├── CMakeLists.txt
    ├── ttyskk.cpp
    ├── ttyskk.conf.in      addon の登録情報
    └── ttyskk-im.conf.in   入力メソッドの登録情報
```

fcitx5 の addon は共有ライブラリ (`libttyskk.so`) と二つの `.conf` を所定の場所へ置く。
`find_package(Fcitx5Core)` と `find_package(Fcitx5Utils)` が要るので、Ubuntu なら
`libfcitx5core-dev` と `fcitx5-modules-dev` (いずれも未導入)。

Rust 側は cargo でビルドし、CMake から呼ぶ (`corrosion` か、単に `add_custom_command`)。

## 段階

1. ~~**capi を作る**~~ — **済**。`capi/` に一式ある。Rust のテスト 9 件に加えて、
   C から共有ライブラリを叩いて変換・候補・確定まで通ることを確かめた
2. ~~**最小の addon**~~ — **済**。`fcitx5/` に一式ある。ブラウザの入力欄で、モード
   切り替え・ローマ字入力・▽ の見出し語・変換と候補送り・確定まで動くことを確かめた
3. ~~**候補窓**~~ — **済**。`CommonCandidateList` に繋いだ。**候補があることと一覧を
   出すことは別**で、SKK では最初の数件を一つずつ送ってから一覧に切り替える。この
   判断は呼ぶ側からは付かないので `ttyskk_candidate_visible` を capi に足した
4. ~~**学習の書き出し**~~ — **済**。確定から 3 秒でタイマーを張って書き出す。入力
   メソッドを離れるときと終了時は待たずに書く。あわせて**端末の側が利用者辞書を
   読み直す**ようにした (`Dict::reload_user`) — 書く側だけ直しても、動いている端末
   には届かないため
5. ~~**モード表示**~~ — **済**。`subMode` で「あ / ア / 半 / Ａ」を出す。あわせて歯車を
   開けるようにし、設定ファイルの場所を案内して開けるようにした

2 に入るには `libfcitx5core-dev` と `fcitx5-modules-dev` が要る (どちらも未導入)。
C++ のビルドなので distrobox の側の作業になる。

### capi を叩いてみる

```sh
cargo build -p ttyskk-capi --release     # target/release/libttyskk.so と capi/ttyskk.h
cargo test -p ttyskk-capi                # C ABI をそのままの形で叩くテスト
```

## 未確定のこと

- 辞書登録 (▽ の候補が無いとき) の見せ方。端末では preedit に `[登録:よみ]` を出して
  いるが、GUI では別の窓を出す実装が多い
- GUI 側は設定と辞書を自分から見張らない。設定は `fcitx5-remote -r` と設定画面の OK で
  読み直せるが、辞書は書き出すときにディスクを読み直すだけ。端末側はどちらも見張って
  いるので、同じようにするか

### 決まったこと

- **学習を書き出す間隔** — 確定から 3 秒。予約が無いときだけ張るので、打ち続けても
  最大 3 秒で書かれる。入力メソッドを離れるときと終了時は待たない
- **候補一覧の頁の切り方** — SKK 流にした。`inline_until` を尊重し、最初の数件は
  一つずつ送る。端末と同じ挙動なので、行き来しても戸惑わない
