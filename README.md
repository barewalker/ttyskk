# ttyskk

端末の中で完結する SKK 日本語入力。

擬似端末で子プロセスを包み、標準入力を横取りして**確定した文字列だけ**を子に渡す。
未確定の文字は端末へ直接重ね描きするので、子アプリの画面には一度も現れない。

入力メソッドが端末アプリの層にいるため、キーは必ず「端末多重化器 → ttyskk → 子」の
順に流れる。X の入力メソッド層 (fcitx5 など) を使ったときのような、`Ctrl+Z` の
取り合いが構造的に起きない。

```sh
ttyskk                  # $SHELL を包む
ttyskk -- claude        # 特定のコマンドを包む
ttyskk vim memo.txt
```

## 作る

```sh
cargo build --release
cargo install --path .
```

依存するのは POSIX の擬似端末と VT100 系のエスケープ列を解する端末だけ。
tmux / screen / SSH / mosh のどれとも組み合わせられるし、なくても動く。
Linux と macOS で動く。**WSL2 なら動くが、Windows ネイティブでは動かない**
(termios と POSIX 擬似端末を使っているため)。

変換には SKK の辞書が要る。

```sh
sudo apt install skkdic     # Debian / Ubuntu / WSL
sudo pacman -S skk-jisyo    # Arch
```

置き場所が違う場合は `TTYSKK_JISYO` で指定する。

## キー操作

### モード切り替え

| キー | 動作 |
|---|---|
| `C-j` | かなモードへ入る |
| `l` | ASCII へ戻る |
| `L` | 全角英数へ |
| `q` | ひらがな ⇄ カタカナ |

モードはカーソルの形と色で分かる。何も打っていない状態でも見て判断できる。

| モード | 形 | 色 |
|---|---|---|
| ASCII | 下線 | 端末の既定 |
| ひらがな | ブロック | 緑 |
| カタカナ | バー | 水色 |
| 全角英数 | 点滅するブロック | 紫 |

**形だけでモードが分かるようにしてある。** 端末多重化器を挟むと色の指定 (OSC 12) が
途中で吸われることがあるため (herdr が実際にそう)、色は補強でしかない。

形は起動している間ずっと設定するので、**カーソルの形が普段と違うこと自体が
「ttyskk が動いている」合図**になる。包んでいなければ端末の既定のままになる。

子アプリがカーソルの形や色を変えた場合 (`vim` など) は、そのつど塗り直して
モードの合図を保つ。`TTYSKK_NO_CURSOR` を設定すると一切触らない。

### 変換

| キー | 動作 |
|---|---|
| 大文字 | 変換の開始 (▽)。`Kanji` → ▽かんじ |
| 途中の大文字 | 送り仮名の始まり。`UgoKu` → ▼動く |
| `space` | 変換する / 次の候補へ |
| `x` | 前の候補へ |
| `C-j` | 候補を確定する |
| `C-g` | 取り消す |
| `q` | ▽ の内容をカタカナにして確定 |
| `Q` | 空の見出し語で変換を始める (複合語向け) |
| `/` | ASCII の見出し語で変換する |
| `BS` | 一文字戻る |

候補が 5 つ目に達すると横並びの一覧が出て、`a s d f j k l` で選べる。
ここに挙げたキーはすべて設定で変えられる (下の「設定」)。

`Enter` は候補の確定だけを行い、改行は送らない。端末では改行が「コマンドの実行」を
意味するため、変換の確定と取り違えると事故になる。

### 辞書登録

候補が見つからないとき、または候補を出し切ったところでもう一度 `space` を押すと
登録に移る。見出しが `[登録:てがき]` の形で出るので、そのまま打ち込んで `Enter`。

| キー | 動作 |
|---|---|
| `Enter` | 登録して確定する |
| `C-g` | 登録をやめて ▽ に戻る |
| `BS` | 一文字消す。空のところで押すと ▽ に戻る |

登録の中でも変換はそのまま使える。未知語が出てくればもう一段積まれ、`[[登録:...]]`
のように括弧が重なる。送りありの語は語幹だけを登録すればよい (`UgoKu` →
`[登録:うご*く]` に「動」と打つと「動く」が出て、辞書には `うごk /動/` が入る)。

長い文字列を登録しておけば、そのままスニペットとして使える。

## 設定

キーの割り当ては `~/.config/ttyskk/config.toml` で変えられる。書いた項目だけが
既定を上書きするので、変えたいものだけ書けばよい。`config.example.toml` が全項目の
一覧を兼ねている。

```toml
[keys]
kana = "C-o"                    # かなモードへ入るキーを変える
cancel = ["C-g", "esc"]         # 複数割り当てるなら並びで書く
select = ["1", "2", "3", "4"]   # 候補一覧から選ぶキー (個数 = 一頁の候補数)

[candidates]
inline = 2                      # 2 つ目の候補から一覧を出す

[behavior]
ascii_keys = ["esc", "C-c"]     # 押すと ASCII モードへ戻るキー
```

キーの書き方は `C-j` / `Ctrl-j` / `ctrl+j`、`space` `enter` `tab` `esc` `bs`、
`q` `/` のような一文字そのもの。場所は `XDG_CONFIG_HOME` を尊重し、`TTYSKK_CONFIG`
で直接指定もできる。

**書き換えは動いている ttyskk にそのまま反映される。** 起動し直さなくてよい。
書き方を間違えた設定は捨てられ、それまでの設定が使われ続ける — 変換の途中で
キーが効かなくなる事態を避けるため。手元で確かめるには次を使う。

```sh
ttyskk --check-config
```

`Enter` だけは割り当てを変えられない。変換中は確定として働き、そうでなければ
そのまま子へ流す。端末では改行が「コマンドの実行」を意味するので、変換の途中で
子へ送るわけにいかない一方、直接入力では必ず届かなければならない。

## vim / nvim と使う

**`Esc` を押すと ASCII モードへ戻る。** 挿入モードを抜けたのにかなモードが残っていると、
次の `dd` や `:w` が日本語になってしまうため。`Esc` は子アプリにもそのまま渡るので、
**vim 側の設定は要らない**。

```sh
ttyskk -- nvim memo.txt
```

`i` で挿入 → `C-j` でかな → 打つ → `Esc` で ASCII、という往復がそのまま回る。
`<C-c>` でも挿入モードを抜ける流儀なら設定で足せる。

```toml
[behavior]
ascii_keys = ["esc", "C-c"]
```

`ascii_keys = []` にすると何もしない。辞書登録の途中では効かない (打ち込んだ内容が
消えると困るため)。

## 二重に起動したとき

すでに ttyskk の中にいる場合、二つ目は自分を子で置き換えて (`exec`) そのまま退く。
包み直しても外側が先にキーを取るので内側は永久に ASCII のまま働かず、辞書をもう一部
抱えるだけになる (常駐が倍) ためで、`TTYSKK_ACTIVE` という目印で判定している。

herdr の既定シェルを `ttyskk` にした状態でそのペインに `ttyskk -- claude` と打つ、
といった場面で効く。承知のうえで入れ子にしたいときは目印を外す。

```sh
env -u TTYSKK_ACTIVE ttyskk -- claude
```

## 拡張鍵盤プロトコルを使うアプリの下で

Claude Code のような TUI は起動時に `CSI > 1 u` (kitty 鍵盤プロトコル) や
`CSI > 4 ; 2 m` (modifyOtherKeys) を有効にする。この状態では `Ctrl+J` が `0x0a`
ではなく `CSI 106;5u` という形で届くため、素朴に読むとモード切り替えが効かない。

ttyskk は SKK が使う `Ctrl+J` と `Ctrl+G` に限ってこの形を解釈する。他のキーは
元のバイト列のまま子へ渡すので、`Ctrl+Z` や `Shift+Enter` など子アプリ側の操作は
そのまま働く。

裏を返すと、**子アプリが `Ctrl+J` / `Ctrl+G` に割り当てている操作は使えなくなる**。
Claude Code で改行を入れたいときは `Shift+Enter` か `\` + `Enter` を使う。

## 辞書

| 種類 | 既定の場所 |
|---|---|
| 共有辞書 | `/usr/share/skk/SKK-JISYO.L`、`/run/host/usr/share/skk/SKK-JISYO.L` |
| 利用者辞書 | `$XDG_DATA_HOME/ttyskk/user.dict` |
| 同梱辞書 | バイナリに埋め込み (`dict/SKK-JISYO.ttyskk`) |

`TTYSKK_JISYO` (`:` 区切り)、`TTYSKK_USER_JISYO` で変えられる。EUC-JP と UTF-8 の
どちらでも読める。

利用者辞書が無い初回に限り、`~/.local/share/fcitx5/skk/user.dict` があれば読み込む。
fcitx5-skk からの乗り換えで学習内容がそのまま引き継がれる。

**丸数字は同梱している。** どの標準辞書にも入っていないため、辞書の設置を待たずに
使えるようバイナリへ埋め込んである。読みは二通りあり、どちらでも同じものが出る。

| 打ち方 | 結果 |
|---|---|
| `Maru1` `space` | ① |
| `C1` `space` | ① |
| `C21` `space` | ㉑ |
| `C50` `space` | ㊿ |

`C` は circle。`Kanji` → ▽かんじ と同じ要領で、大文字で始めると見出し語が `c1` に
なる。数字が続くのでローマ字とは衝突しない。共有辞書に同じ見出し語があればそちらが
優先される。

候補が空、または半角の空白だけの項目は読み飛ばす。登録に失敗した跡としてしばしば
利用者辞書に残っており、共有辞書の正しい候補を覆い隠すため。全角空白は正当な候補
なので残す。

確定した候補は先頭へ移り、終了時に保存される。保存は「この起動で覚えたこと」だけを
ディスクの内容に重ねる方式なので、複数の端末で同時に使っても学習を消し合わない。

mosh 越しに使う場合はリモート側で動くため、辞書と学習がそこに集まる。どの端末から
繋いでも同じ変換になる。

## 設計

- **画面の内容には触らない** — 書くのは重ね描きとカーソルの見た目だけで、格子の
  中身は一切書き換えない。ASCII モードで書くのはカーソルの形と色 (モードの合図)
  だけなので、`less` のような全画面アプリの表示は乱れない。`TTYSKK_NO_CURSOR` を
  設定すると本当に 1 バイトも書かなくなり、素の実行とバイト単位で一致する
  (起動時のカーソル位置の問い合わせだけは残るが、画面には現れない)。
- **常設の行を作らない** — 最下行にモード表示を置かない。端末のスクロール領域に
  触れないので、子アプリの画面配置を乱さない。
- **未確定文字は重ね描き** — 子アプリには送らないので、変換を取り消しても子の
  側には何も残らない。
- **カーソル位置は起動時に一度だけ端末へ尋ねる** — 画面の途中で起動しても重ね描き
  の基準が合うように、子を起こす前に `CSI 6n` を送る。このときは子がまだ何も
  出力していないので、応答が子の出力と混ざらない。以降は子の出力を横から読んで
  画面の控え (`src/screen.rs`) を保ち、二度と尋ねない。例外は画面サイズの変更後で、
  折り返しが組み直されてカーソルの絶対位置が変わるため尋ね直す (子が描き直し
  始めたあとに届いた応答は古いものとして捨てる)。
- **エスケープ列の途中には割り込まない** — 子の出力が読み取り境界で切れている
  間は重ね描きを控える (`src/input.rs` の `SeqTracker`)。

### 部品

| ファイル | 役割 |
|---|---|
| `src/main.rs` | 擬似端末と入出力の仲介 |
| `src/screen.rs` | 画面の控え (文字と表示属性の格子) |
| `src/render.rs` | 重ね描きと、控えからの書き戻し |
| `src/skk.rs` | SKK の状態機械 |
| `src/romaji.rs` | ローマ字からかなへの変換表 |
| `src/dict.rs` | 辞書の読み込み・引き当て・学習 |
| `src/input.rs` | 入力バイト列のキーへの切り出し |
| `src/config.rs` | 設定ファイルの読み込みと見張り |

## まだ無いもの

- 候補窓を浮かせる表示 (いまは行内に横並び)
- 補完 (`TAB`)、数字変換、半角カタカナ
- 変換中のカーソル移動 (矢印キーは見出し語を確定してから子へ渡す)

## ライセンス

MIT または Apache-2.0 のどちらかを選べる (`LICENSE-MIT` / `LICENSE-APACHE`)。

先行実装の [sentimental-skk](https://github.com/saitoha/sentimental-skk) (GPL-3.0) は
設計の参考にしたが、**移植ではない**。`NOTES.md` にあるのは読み解いた内容を説明する
ための短い引用で、コードは Rust で新しく書いている。

## English

`ttyskk` is an SKK Japanese input method that lives entirely inside the terminal.
It wraps a child process in a pseudo-terminal, intercepts stdin, and passes only
*confirmed* text to the child. Unconfirmed text is painted directly onto the
terminal as an overlay, so it never enters the child application's screen at all.

Because the input method sits at the terminal-application layer, keys always flow
in the order "multiplexer → ttyskk → child". This removes, structurally, the key
contention you get when an X-level input method (fcitx5 and friends) fights your
terminal multiplexer over `Ctrl+Z`.

Documentation is in Japanese, since the users are.
