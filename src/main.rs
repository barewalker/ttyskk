//! ttyskk — 端末の中で完結する SKK 日本語入力。
//!
//! 子プロセスを擬似端末で包み、標準入力を横取りして確定した文字列だけを渡す。
//! 未確定の文字は端末へ直接重ね描きするので、子アプリの画面には現れない。

// 端末に載せる部分だけがここにある。変換エンジンは lib (ttyskk) の側。
mod input;
mod render;
mod screen;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use input::Decoder;
use render::Overlay;
use screen::Screen;
use ttyskk::config::{self, Config, Marker};
use ttyskk::dict::Dict;
use ttyskk::skk::{Mode, Skk};

use ttyskk::config::EXAMPLE as CONFIG_EXAMPLE;

const USAGE: &str = "\
ttyskk — 端末の中で完結する SKK 日本語入力

使い方:
    ttyskk [オプション] [--] [コマンド [引数...]]

コマンドを省くと $SHELL を起動する。

オプション:
    -h, --help        この使い方を表示する
    -V, --version     版を表示する
    --check-config    設定ファイルを検査して終わる
    --config-example  設定の見本を書き出す (全項目を既定値のまま # で無効にしたもの)

環境変数:
    TTYSKK_JISYO       共有辞書のパス (`:` 区切り)
    TTYSKK_USER_JISYO  利用者辞書のパス
    TTYSKK_CONFIG      設定ファイルのパス (既定 ~/.config/ttyskk/config.toml)
    TTYSKK_NO_CURSOR   モードに応じたカーソルの形・色の変更をやめる
    TTYSKK_ACTIVE      ttyskk の中にいる印。あるときは包まずに子をそのまま起こす
    TTYSKK_DEBUG       不具合を追う記録の書き出し先

キー操作 (かなモード):
    C-j        かなモードへ入る / 入力中のローマ字を確定する
    l  L  q    ASCII / 全角英数 / カタカナ へ切り替える
    大文字     変換の開始 (▽)。途中の大文字は送り仮名の始まり
    space      変換する / 次の候補へ
    x          前の候補へ
    C-j        候補を確定する
    C-g        取り消す
    /          ASCII 見出し語で変換する
";

enum Event {
    Child(Vec<u8>),
    ChildEof,
    Input(Vec<u8>),
    Winch,
    /// 設定ファイルが書き換わって読み直せた
    Reconfigured(Box<Config>),
}

/// 端末を raw モードにし、終了時に必ず元へ戻す。
struct RawGuard {
    original: nix::sys::termios::Termios,
}

impl RawGuard {
    fn new() -> Result<Self> {
        use nix::sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr};
        let stdin = std::io::stdin();
        let original = tcgetattr(&stdin).context("端末属性を取得できない")?;
        let mut raw = original.clone();
        cfmakeraw(&mut raw);
        tcsetattr(&stdin, SetArg::TCSANOW, &raw).context("端末を raw モードにできない")?;
        Ok(RawGuard { original })
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        use nix::sys::termios::{SetArg, tcsetattr};
        let _ = tcsetattr(std::io::stdin(), SetArg::TCSANOW, &self.original);
    }
}

fn winsize() -> (u16, u16) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &raw mut ws) };
    if rc != 0 || ws.ws_row == 0 || ws.ws_col == 0 {
        (24, 80)
    } else {
        (ws.ws_row, ws.ws_col)
    }
}

fn default_system_jisyo() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var("TTYSKK_JISYO") {
        return v
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
    }
    // distrobox の中からはホスト側が /run/host 以下に見える
    [
        "/usr/share/skk/SKK-JISYO.L",
        "/run/host/usr/share/skk/SKK-JISYO.L",
    ]
    .iter()
    .map(PathBuf::from)
    .collect()
}

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn user_jisyo() -> PathBuf {
    std::env::var_os("TTYSKK_USER_JISYO")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_home().join("ttyskk/user.dict"))
}

/// 端末にカーソル位置を尋ねる (DSR 6)。
fn request_cursor_report(out: &mut impl Write) {
    let _ = out.write_all(b"\x1b[6n");
    let _ = out.flush();
}

/// カーソル位置の報告を待つ。
///
/// 画面の控えは「いまカーソルがどこにあるか」を基準に始まる。ここを (1,1) と
/// 決め打ちすると、画面の途中で ttyskk を起動したときに重ね描きが最上段へ出て
/// しまう。子プロセスを起こす前に一度だけ尋ねるので、応答が子の出力と混ざらない。
///
/// 戻り値は位置と、報告以外に読めたバイト列 (先走って打たれた文字)。
fn read_cursor_report(timeout: Duration) -> (Option<(usize, usize)>, Vec<u8>) {
    let deadline = Instant::now() + timeout;
    let mut buf = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return (None, buf);
        }
        let mut pfd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&raw mut pfd, 1, remaining.as_millis() as libc::c_int) } <= 0 {
            return (None, buf);
        }
        let mut chunk = [0u8; 64];
        let n = unsafe {
            libc::read(
                libc::STDIN_FILENO,
                chunk.as_mut_ptr() as *mut libc::c_void,
                chunk.len(),
            )
        };
        if n <= 0 {
            return (None, buf);
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        if let Some((pos, rest)) = input::take_cursor_report(&buf) {
            return (Some(pos), rest);
        }
    }
}

/// モードを表すカーソルの見た目。形 (DECSCUSR) と色 (OSC 12/112) の組。
///
/// 起動している間はどのモードでも設定するので、「カーソルの形が普段と違う」こと
/// 自体が ttyskk が動いている合図になる。
///
/// **色だけに頼らず、形だけでモードが分かるようにしてある。** 端末多重化器を
/// 挟むと OSC 12 が途中で吸われて色が変わらないことがある (herdr が実際にそう)。
/// DECSCUSR は 3 つの形と点滅の有無しか持たないので、よく使う 3 つのモードに
/// 形を割り当て、めったに使わない全角英数だけ点滅で区別する。
fn cursor_indicator(mode: Mode, marker: Marker) -> &'static str {
    match marker {
        // カーソルの真下に色を敷く方式では、ブロックのカーソルがその色を覆う。
        // 形は下線に固定し、モードは色だけで表す (形 = 動いている合図)。
        Marker::Cell | Marker::Symbol => "\x1b[4 q\x1b]112\x07",
        // 右隣に色の箱を置く方式では、カーソル自体には色を付けない。
        // 付けると色付きのものが二つ並んで見えて紛らわしい。色は箱が担い、
        // カーソルは形だけでモードを表す。
        Marker::Beside => match mode {
            Mode::Ascii => "\x1b[4 q\x1b]112\x07",           // 固定の下線
            Mode::Hiragana => "\x1b[2 q\x1b]112\x07",        // 固定のブロック
            Mode::Katakana => "\x1b[6 q\x1b]112\x07",        // 固定のバー
            Mode::HankakuKatakana => "\x1b[5 q\x1b]112\x07", // 点滅するバー
            Mode::ZenkakuAscii => "\x1b[1 q\x1b]112\x07",    // 点滅するブロック
        },
        // 印を出さない / 文字で出す場合は、カーソルの色も手掛かりとして使う
        _ => match mode {
            Mode::Ascii => "\x1b[4 q\x1b]112\x07",           // 固定の下線
            Mode::Hiragana => "\x1b[2 q\x1b]12;#7fd75f\x07", // 固定のブロック
            Mode::Katakana => "\x1b[6 q\x1b]12;#5fd7ff\x07", // 固定のバー
            Mode::HankakuKatakana => "\x1b[5 q\x1b]12;#5fafaf\x07", // 点滅するバー
            Mode::ZenkakuAscii => "\x1b[1 q\x1b]12;#d787ff\x07", // 点滅するブロック
        },
    }
}

/// カーソルを端末の既定へ戻す (形・色とも)。
const CURSOR_RESET: &[u8] = b"\x1b[0 q\x1b]112\x07";

/// すでに ttyskk の中にいることを子へ知らせる目印。
const ACTIVE_ENV: &str = "TTYSKK_ACTIVE";

/// 不具合を追うための記録。`TTYSKK_DEBUG` にパスを渡したときだけ書く。
///
/// 重ね描きは「控えのカーソル位置」に全面的に依存する。ずれた瞬間を後から
/// 突き止められるよう、位置と描いた範囲を追えるようにしておく。
struct Trace(Option<std::fs::File>);

impl Trace {
    fn new() -> Self {
        Trace(std::env::var_os("TTYSKK_DEBUG").and_then(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
        }))
    }

    fn log(&mut self, args: std::fmt::Arguments) {
        if let Some(f) = self.0.as_mut() {
            let _ = writeln!(f, "{args}");
            let _ = f.flush();
        }
    }

    fn on(&self) -> bool {
        self.0.is_some()
    }
}

fn main() -> Result<()> {
    // 最初の引数だけを見て、それ以降は丸ごと子のコマンド行として扱う
    let mut iter = std::env::args().skip(1);
    let mut command: Vec<String> = Vec::new();
    if let Some(first) = iter.next() {
        match first.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("ttyskk {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--config-example" => {
                print!("{CONFIG_EXAMPLE}");
                return Ok(());
            }
            "--check-config" => {
                let path = config::config_path();
                Config::load(&path).with_context(|| format!("{}", path.display()))?;
                if path.exists() {
                    println!("ttyskk: {} に問題なし", path.display());
                } else {
                    println!("ttyskk: {} は無い (既定で動く)", path.display());
                }
                return Ok(());
            }
            "--" => {}
            _ => command.push(first),
        }
        command.extend(iter);
    }
    if command.is_empty() {
        command.push(std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()));
    }

    // すでに ttyskk の中にいるなら、包み直さず子をそのまま起こす。
    //
    // 外側が先にキーを取るので内側は永久に ASCII のままで一度も働かず、辞書を
    // もう一部抱えるだけになる (常駐が倍)。exec で自分自身を置き換えるので、
    // 余分なプロセスも残らない。承知のうえで入れ子にしたいときは
    // `env -u TTYSKK_ACTIVE ttyskk ...` と唱える。
    if std::env::var_os(ACTIVE_ENV).is_some() {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&command[0])
            .args(&command[1..])
            .exec();
        return Err(anyhow::Error::new(err).context(format!("{} を起こせない", command[0])));
    }

    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
        bail!("標準入力が端末ではない");
    }

    let import = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".local/share/fcitx5/skk/user.dict"));
    let dict = Dict::load(&default_system_jisyo(), user_jisyo(), import.as_deref())?;
    if dict.system_len() == 0 {
        eprintln!(
            "ttyskk: 共有辞書が見つからない。TTYSKK_JISYO でパスを指定できる。変換はできないが起動は続ける。"
        );
    }
    // 設定は子を起こす前に読む。ここで駄目なら画面を触る前に知らせられる。
    let config_path = config::config_path();
    let cfg = Config::load(&config_path)
        .with_context(|| format!("設定 {} を読めない", config_path.display()))?;
    let mut skk = Skk::new(dict, cfg);

    let (rows, cols) = winsize();

    // 子を起こす前に、raw モードにしてカーソル位置を尋ねる。
    // 応答は必ずこちらの stdin に来るので、子の出力と混ざる余地がない。
    let raw = RawGuard::new()?;
    let mut stdout = std::io::stdout();
    request_cursor_report(&mut stdout);
    // 応答が来るまで待つ。mosh や遅い経路では往復に時間がかかり、300ms では
    // 取りこぼす。取りこぼすと控えの原点がずれ、重ね描きの消去が無関係のセルを
    // 潰したうえ、物理カーソルも誤った場所へ動くのでシェルの出力までそこへ落ちる。
    // 答えが来た時点で戻るので、速い端末では待ち時間にならない。
    let (origin, typeahead) = read_cursor_report(Duration::from_millis(1500));

    let mut trace = Trace::new();
    trace.log(format_args!(
        "--- 起動 画面 {rows}x{cols} 位置の報告 {origin:?} 先走り {} バイト",
        typeahead.len()
    ));

    let mut screen = Screen::new(rows as usize, cols as usize);
    match origin {
        Some((r, c)) => screen.set_cursor(r, c),
        // 答えない端末では、プロンプトが画面最下段にある場合が多いとみて左下に置く
        None => screen.set_cursor(rows as usize - 1, 0),
    }

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("擬似端末を開けない")?;

    let mut cmd = CommandBuilder::new(&command[0]);
    for a in &command[1..] {
        cmd.arg(a);
    }
    for (k, v) in std::env::vars_os() {
        cmd.env(k, v);
    }
    // 子の中でうっかり ttyskk を起こしても包み直さないための目印
    cmd.env(ACTIVE_ENV, "1");
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("起動できない: {}", command[0]))?;
    drop(pair.slave);

    let mut writer = pair.master.take_writer().context("擬似端末に書けない")?;
    let mut reader = pair
        .master
        .try_clone_reader()
        .context("擬似端末を読めない")?;

    let (tx, rx): (Sender<Event>, Receiver<Event>) = channel();

    // 子の出力
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(Event::Child(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = tx.send(Event::ChildEof);
        });
    }

    // 利用者の入力
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(Event::Input(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }

    // 端末の大きさの変化
    {
        let tx = tx.clone();
        let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGWINCH])
            .context("SIGWINCH を捕まえられない")?;
        std::thread::spawn(move || {
            for _ in signals.forever() {
                if tx.send(Event::Winch).is_err() {
                    break;
                }
            }
        });
    }

    // 設定ファイルの書き換えを見張る
    config::watch_into(config_path, tx.clone(), |c| {
        Event::Reconfigured(Box::new(c))
    });

    // 位置を尋ねている間に打たれた文字は、まだ処理していないので改めて流し込む
    if !typeahead.is_empty() {
        let _ = tx.send(Event::Input(typeahead));
    }

    let mut parser = vte::Parser::new();
    let mut overlay = Overlay::new();
    let mut decoder = Decoder::new(skk.config());
    let mut tracker = input::SeqTracker::new();
    let mut mid_sequence = false;
    // 尋ねたカーソル位置の報告を待っているか
    let mut awaiting_report = false;
    // 一度でもカーソル位置の報告を受け取れたか。
    //
    // 受け取れていないと控えの原点が当てずっぽうになる。その状態で重ね描きすると
    // 無関係のセルを空白で潰し、物理カーソルも誤った場所へ動くのでシェルの出力まで
    // そこへ落ちる。**壊すくらいなら描かない。** 位置が分かった時点で描き始める。
    let mut anchored = origin.is_some();
    let show_cursor_color = std::env::var_os("TTYSKK_NO_CURSOR").is_none();

    // 端末に何か書いたか。何も書いていなければ後片付けも要らない (完全透過を保つ)。
    let mut touched = false;

    // 子に奪われたカーソルの見た目を塗り直す必要があるか
    let mut cursor_dirty = false;

    // 起動した時点でカーソルを ttyskk のものにする。何も打っていなくても、
    // 動いていること・いまどのモードかが見て分かるようにするため。
    if show_cursor_color {
        let _ = stdout.write_all(cursor_indicator(skk.mode, skk.marker()).as_bytes());
        let _ = stdout.flush();
        touched = true;
    }

    for ev in rx {
        match ev {
            Event::Child(data) => {
                let had_overlay = !overlay.is_empty();
                // 消去は直前の切れ目 (安全な位置) で行う
                let mut out = overlay.erase(&screen);
                if !out.is_empty() {
                    // 書き戻しはカーソルと表示属性を動かす。子は自分が居た場所に
                    // 書くつもりなので、出力を流す前に必ず戻す。戻さないと子の
                    // 文字が一つずれた場所に落ち、控えと画面が食い違っていく。
                    out.extend(Overlay::restore_terminal(&screen));
                }
                parser.advance(&mut screen, &data);
                out.extend_from_slice(&data);
                // 出力が列の途中で切れているあいだは割り込まない
                mid_sequence = tracker.feed(&data);
                if trace.on() {
                    trace.log(format_args!(
                        "子 {:3} バイト → 控え ({},{}) 途中={} {:?}",
                        data.len(),
                        screen.row,
                        screen.col,
                        mid_sequence,
                        String::from_utf8_lossy(&data[..data.len().min(60)])
                    ));
                }
                // 子がカーソルの形や色を変えたら、モードの合図を塗り直す。
                // 列の途中では割り込めないので、書けるようになるまで持ち越す。
                cursor_dirty |= screen.take_cursor_style_touched();
                if cursor_dirty && !mid_sequence && show_cursor_color {
                    out.extend_from_slice(cursor_indicator(skk.mode, skk.marker()).as_bytes());
                    cursor_dirty = false;
                }
                // 子が描き直したので、待っていた位置報告はもう古い
                awaiting_report = false;
                let preedit = skk.preedit();
                if !preedit.is_empty() && !mid_sequence && anchored {
                    out.extend(overlay.draw(&screen, &preedit));
                    out.extend(Overlay::restore_terminal(&screen));
                    touched = true;
                } else if had_overlay {
                    out.extend(Overlay::restore_terminal(&screen));
                }
                stdout.write_all(&out)?;
                stdout.flush()?;
            }
            Event::Input(mut data) => {
                // 画面の大きさが変わった直後は、尋ねておいた位置報告が紛れている
                if awaiting_report && let Some((pos, rest)) = input::take_cursor_report(&data) {
                    screen.set_cursor(pos.0, pos.1);
                    awaiting_report = false;
                    anchored = true;
                    data = rest;
                }
                let mut to_child = Vec::new();
                let mut mode_changed = false;
                for key in decoder.feed(&data) {
                    let r = skk.handle(key);
                    to_child.extend(r.to_child());
                    mode_changed |= r.mode_changed;
                }
                let had_overlay = !overlay.is_empty();
                // 子の反響が届く前に重ね描きを消しておく
                let mut out = overlay.erase(&screen);
                if !to_child.is_empty() {
                    writer.write_all(&to_child)?;
                    writer.flush()?;
                }
                if mode_changed && show_cursor_color {
                    out.extend_from_slice(cursor_indicator(skk.mode, skk.marker()).as_bytes());
                    touched = true;
                }
                let preedit = skk.preedit();
                if trace.on() {
                    trace.log(format_args!(
                        "鍵 → 控え ({},{}) 錨={} 子へ {:?} 重ね描き {:?}",
                        screen.row,
                        screen.col,
                        anchored,
                        String::from_utf8_lossy(&to_child),
                        preedit
                            .at_cursor
                            .iter()
                            .map(|s| s.text.as_str())
                            .collect::<String>()
                    ));
                }
                if !preedit.is_empty() && !mid_sequence && anchored {
                    out.extend(overlay.draw(&screen, &preedit));
                    out.extend(Overlay::restore_terminal(&screen));
                    touched = true;
                } else if had_overlay {
                    out.extend(Overlay::restore_terminal(&screen));
                }
                if !out.is_empty() {
                    stdout.write_all(&out)?;
                    stdout.flush()?;
                }
            }
            Event::Winch => {
                let (r, c) = winsize();
                let _ = pair.master.resize(PtySize {
                    rows: r,
                    cols: c,
                    pixel_width: 0,
                    pixel_height: 0,
                });
                screen.resize(r as usize, c as usize);
                // 座標が意味を失うので、消さずに忘れる (子が描き直す)
                overlay.forget();
                // 折り返しが組み直されてカーソルの絶対位置が変わる。改めて尋ねる。
                // 子が描き直し始めたら報告は古くなるので、その場合は捨てる。
                request_cursor_report(&mut stdout);
                awaiting_report = true;
            }
            Event::Reconfigured(cfg) => {
                // 復号の対象は設定から作るので、切り出す側にも同じ設定を渡す
                decoder.set_config(&cfg);
                skk.set_config(*cfg);
            }
            Event::ChildEof => break,
        }
    }

    // 後片付け: 重ね描きを消し、カーソル色と表示属性を戻す。
    // 一度も書いていなければ何もしない (英数のまま使い終えた場合)。
    if touched {
        let mut out = overlay.erase(&screen);
        if show_cursor_color {
            out.extend_from_slice(CURSOR_RESET);
        }
        out.extend_from_slice(b"\x1b[0m\x1b[?25h");
        let _ = stdout.write_all(&out);
        let _ = stdout.flush();
    }

    if let Err(e) = skk.dict_mut().save() {
        eprintln!("ttyskk: 利用者辞書を保存できない: {e}");
    }

    let status = child.wait().context("子プロセスの終了を待てない")?;
    drop(raw);
    std::process::exit(status.exit_code() as i32);
}
