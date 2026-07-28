//! ttyskk — 端末の中で完結する SKK 日本語入力。
//!
//! 子プロセスを擬似端末で包み、標準入力を横取りして確定した文字列だけを渡す。
//! 未確定の文字は端末へ直接重ね描きするので、子アプリの画面には現れない。

// 端末に載せる部分だけがここにある。変換エンジンは lib (ttyskk) の側。
mod input;
mod render;
mod screen;

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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
    --import <辞書>   別の SKK 辞書を利用者辞書に取り込む (他の実装からの移行)
    --edit-snippets [見出し語]
                      定型文を $EDITOR で編集する。新しい項目の雛形を末尾に足し、
                      その行を開く。見出し語を渡すとそれを埋めておく

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
    /// 利用者辞書が別のプロセス (GUI の入力メソッドなど) に書き換えられた
    DictChanged,
    /// スニペットが編集器で書き換えられた
    SnippetsChanged,
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

impl RawGuard {
    /// 元の設定へ戻す。編集器のように、自分で端末を整えるものを起こす間だけ。
    fn suspend(&self) {
        use nix::sys::termios::{SetArg, tcsetattr};
        let _ = tcsetattr(std::io::stdin(), SetArg::TCSANOW, &self.original);
    }

    /// raw モードへ戻す。
    fn resume(&self) {
        use nix::sys::termios::{SetArg, cfmakeraw, tcsetattr};
        let mut raw = self.original.clone();
        cfmakeraw(&mut raw);
        let _ = tcsetattr(std::io::stdin(), SetArg::TCSANOW, &raw);
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

/// スニペットの置き場所。設定が空なら既定の一つだけを読む。
///
/// 既定を利用者辞書と同じところに置くので、辞書を git に載せて職場と自宅で
/// 分け合っている場合、定型文もそのまま行き来する。
fn snippet_paths(cfg: &Config) -> Vec<PathBuf> {
    if cfg.snippets.is_empty() {
        vec![data_home().join("ttyskk/snippets.code-snippets")]
    } else {
        cfg.snippets.clone()
    }
}

/// 定型文を編集器で開く。新しい項目の雛形を末尾に足し、その行から始める。
///
/// **書き足す場所を探すところから始めずに済ませる**ためのもの。一覧が目の前に
/// あるので、見出し語を先に決めなくてよいし、既にある定型文を見ながら足せる。
/// 手つかずのまま閉じたら雛形は残さない。
fn edit_snippets(prefix: &str) -> Result<Option<String>> {
    let cfg = Config::load(&config::config_path())?;
    let path = snippet_paths(&cfg)
        .into_iter()
        .next()
        .expect("置き場所は必ず一つ以上ある");

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let before = fs::read_to_string(&path).unwrap_or_default();
    let (with_template, line) = ttyskk::snippet::append_template(&before, prefix);
    fs::write(&path, &with_template).with_context(|| format!("{} に書けない", path.display()))?;

    // 指定された編集器が無くて別のものに落ちたら、その旨が返る
    let note = match open_editor(&path, line) {
        Ok(note) => note,
        Err(e) => {
            // **開けなかったのだから、足した雛形は要らない。** 片付けずに戻ると、
            // 試すたびに空の項目が溜まっていく。
            let _ = fs::write(&path, &before);
            return Err(e);
        }
    };

    let after = fs::read_to_string(&path)?;
    if after == with_template {
        // 何も書かずに閉じた。足した雛形を片付ける。
        fs::write(&path, &before)?;
        return Ok(note.or_else(|| Some("変更なし".into())));
    }
    match ttyskk::snippet::parse(&after) {
        Ok(list) => {
            let done = format!("{} に {} 語", path.display(), list.len());
            Ok(Some(match note {
                Some(n) => format!("{n}。{done}"),
                None => done,
            }))
        }
        // 直すのは書いた本人なので、消したり戻したりはしない
        Err(e) => Err(e).with_context(|| format!("{} を読み直せない", path.display())),
    }
}

/// `read` を中断させるためだけの合図。何もしない。
extern "C" fn wake_the_reader(_: libc::c_int) {}

/// `SIGUSR1` を「握りつぶすが `read` は中断させる」形で捕まえる。
///
/// 既定のままだとプロセスが終わってしまう。`SA_RESTART` を付けないので、
/// 受けた `read` は `EINTR` で戻る。
fn catch_the_wakeup_signal() {
    unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = wake_the_reader as extern "C" fn(libc::c_int) as usize;
        sa.sa_flags = 0;
        libc::sigemptyset(&mut sa.sa_mask);
        libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut());
    }
}

/// 標準入力を読む係を、外から一時的に止めるための門。
///
/// 編集器のように**自分で端末を読むもの**を起こす間は、ttyskk が読むのをやめないと
/// 打鍵を横取りしてしまう。端末は一つしかないので、読む側が二人いると奪い合いになる。
///
/// 止めるときは、係が本当に手を離したことを確かめてから進む。`read` に入った直後に
/// 止めても、その一回は端末を掴んだままだから。
#[derive(Default)]
struct InputGate {
    closed: std::sync::atomic::AtomicBool,
    /// 係が手を離しているか
    idle: std::sync::atomic::AtomicBool,
}

impl InputGate {
    /// 読む係が呼ぶ。閉じている間は待つ。待ったなら true。
    fn wait_while_closed(&self) -> bool {
        use std::sync::atomic::Ordering;
        if !self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.idle.store(true, Ordering::Release);
        while self.closed.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.idle.store(false, Ordering::Release);
        true
    }

    /// 門を閉じ、係が手を離すまで待つ。
    ///
    /// 係が `read` の途中なら、その一回が終わるまでは離せない。**打鍵を一つ待つ**
    /// ことになるので、シグナルを送って `read` を中断させる。
    fn close(&self, reader: Option<libc::pthread_t>) {
        use std::sync::atomic::Ordering;
        self.closed.store(true, Ordering::Release);
        if let Some(t) = reader {
            // SIGUSR1 は握りつぶす向きで登録してあるので、read が EINTR で戻るだけ
            unsafe { libc::pthread_kill(t, libc::SIGUSR1) };
        }
        for _ in 0..200 {
            if self.idle.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// 門を開ける。
    fn open(&self) {
        self.closed
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

/// いまの地方時。定型文の `$CURRENT_YEAR` などを開くのに渡す。
///
/// 地方時への直しは libc に任せる。エンジンの側は端末にも GUI にも載せられるよう
/// libc を持たないので、時計はこちらから教える。
fn local_now() -> ttyskk::snippet::Now {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // 環境変数 TZ を見て地方時に直す。失敗しても既定の 0 のまま進む。
    unsafe { libc::localtime_r(&secs, &mut tm) };
    ttyskk::snippet::Now {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
        weekday: tm.tm_wday as u32,
        // time_t の幅は環境によって違う。いまの対象では i64 と同じなので
        // clippy は無駄と言うが、幅の違う環境のために残す。
        #[allow(clippy::unnecessary_cast)]
        unix: secs as i64,
    }
}

/// 画面を明け渡して編集器を起こし、終わったら元の見た目へ戻す。
///
/// 子プロセスは動いたままなので、**画面をどう返すかが要**になる。子が主画面にいる
/// なら副画面で編集器を動かし、抜ければ端末が元の内容を戻してくれる。子が既に副画面に
/// いる (vim など) 場合はその手が使えないので、控えから描き直す。
///
/// 失敗しても入力を続けられるようにする。ここで抜けると、包んでいる子ごと道連れになる。
#[allow(clippy::too_many_arguments)]
fn run_editor_over_the_screen(
    raw: &RawGuard,
    gate: &InputGate,
    reader: Option<libc::pthread_t>,
    stdout: &mut impl Write,
    screen: &Screen,
    word: &str,
    trace: &mut Trace,
) {
    // 端末を読む係を止める。**止めないと打鍵を横取りして編集器に何も届かない。**
    gate.close(reader);
    // 子が主画面にいるなら、副画面を借りれば端末が元に戻してくれる
    let borrow_alt = !screen.alt_screen;
    if borrow_alt {
        let _ = stdout.write_all(b"\x1b[?1049h");
    }
    let _ = stdout.write_all(b"\x1b[H\x1b[2J");
    let _ = stdout.flush();

    raw.suspend();
    let result = edit_snippets(word);
    // **しくじったら必ず知らせる。** 画面を返してしまうと何も起きなかったように
    // 見えて、利用者は原因に辿り着けない (記録を取らない限り分からない)。
    // 画面はまだこちらのものなので、戻す前に書く。
    match &result {
        Ok(None) => {}
        Ok(Some(note)) => tell_before_returning(stdout, &format!("ttyskk: {note}"), false),
        Err(e) => tell_before_returning(stdout, &format!("ttyskk: {e:#}"), true),
    }

    raw.resume();
    gate.open();

    if borrow_alt {
        let _ = stdout.write_all(b"\x1b[?1049l");
    } else {
        let _ = stdout.write_all(screen.repaint().as_bytes());
    }
    let _ = stdout.flush();

    match result {
        Ok(_) => trace.log(format_args!("--- 定型文を編集した ({word})")),
        Err(e) => trace.log(format_args!("--- 定型文を編集できない: {e:#}")),
    }
}

/// 画面を返す前に一言知らせる。`wait` なら打鍵を一つ待つ。
///
/// 端末を読む係は止めたままなので、ここでは自分で読む。
fn tell_before_returning(stdout: &mut impl Write, msg: &str, wait: bool) {
    let _ = write!(stdout, "\r\n{msg}\r\n");
    if !wait {
        let _ = stdout.flush();
        std::thread::sleep(Duration::from_millis(900));
        return;
    }
    let _ = write!(stdout, "\r\n何かキーを押すと戻ります…");
    let _ = stdout.flush();

    let mut pfd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    // 誰も押さないまま置き去りにされることもあるので、待ちきりにはしない
    if unsafe { libc::poll(&raw mut pfd, 1, 15_000) } > 0 {
        let mut buf = [0u8; 64];
        unsafe {
            libc::read(
                libc::STDIN_FILENO,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
    }
}

/// 編集器を起動して待つ。行を指定できる編集器には行も渡す。
///
/// `$VISUAL` → `$EDITOR` → `vi` の順に試し、**見つからないものは飛ばして次へ落とす**。
/// 端末の中と外で入っているものが違うことがあり (distrobox の中には nvim があるが
/// ホストには無い、など)、指定が空振りするだけで何も書けなくなるのは困る。
///
/// 落としたことは呼んだ側へ返す。黙って別の編集器が開くと驚くので、後で知らせる。
fn open_editor(path: &Path, line: usize) -> Result<Option<String>> {
    let mut missing = Vec::new();
    let candidates = [
        ("VISUAL", std::env::var("VISUAL").ok()),
        ("EDITOR", std::env::var("EDITOR").ok()),
        ("既定", Some("vi".to_string())),
    ];
    for (source, spec) in candidates {
        let Some(spec) = spec.filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        match run_editor(&spec, path, line) {
            Ok(()) => {
                return Ok((!missing.is_empty())
                    .then(|| format!("{} が無いので {spec} で開いた", missing.join(" と "))));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing.push(format!("{source}={spec}"));
            }
            Err(e) => bail!("{spec} を起こせない: {e}"),
        }
    }
    bail!(
        "編集器が見つからない ({})。$EDITOR にこの環境で使えるものを指定する",
        missing.join(", ")
    )
}

/// 編集器を一つ起こして終わりまで待つ。
fn run_editor(spec: &str, path: &Path, line: usize) -> std::io::Result<()> {
    // `EDITOR="code -w"` のように引数付きで書かれることがある
    let mut parts = spec.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let args: Vec<&str> = parts.collect();

    let mut cmd = std::process::Command::new(program);
    cmd.args(&args);
    // `+N` は vi 系・emacs・nano・helix・kakoune で通じる。知らないものには渡さない
    // (引数として解されずファイル名と見なされると、空のファイルを作ってしまう)。
    let name = Path::new(program)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(program);
    if matches!(
        name,
        "vi" | "vim" | "nvim" | "view" | "emacs" | "emacsclient" | "nano" | "hx" | "helix" | "kak"
    ) {
        cmd.arg(format!("+{line}"));
    }
    cmd.arg(path);

    let status = cmd.status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!("{status} で終わった")));
    }
    Ok(())
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
            "--import" => {
                let Some(path) = iter.next() else {
                    bail!("--import には取り込む辞書のパスが要る");
                };
                let path = PathBuf::from(path);
                let mut dict = Dict::load(&[], user_jisyo(), None)?;
                let added = dict
                    .import_user(&path)
                    .with_context(|| format!("{} を取り込めない", path.display()))?;
                println!(
                    "ttyskk: {} から {added} 件を {} へ取り込んだ",
                    path.display(),
                    user_jisyo().display()
                );
                return Ok(());
            }
            "--edit-snippets" => {
                if let Some(note) = edit_snippets(iter.next().as_deref().unwrap_or(""))? {
                    println!("ttyskk: {note}");
                }
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
    let mut dict = Dict::load(&default_system_jisyo(), user_jisyo(), import.as_deref())?;
    if dict.system_len() == 0 {
        eprintln!(
            "ttyskk: 共有辞書が見つからない。TTYSKK_JISYO でパスを指定できる。変換はできないが起動は続ける。"
        );
    }
    // 設定は子を起こす前に読む。ここで駄目なら画面を触る前に知らせられる。
    let config_path = config::config_path();
    let cfg = Config::load(&config_path)
        .with_context(|| format!("設定 {} を読めない", config_path.display()))?;

    // 定型文。手で書くものなので、壊れていても起動は続けてここで知らせる
    // (画面を触る前でないと、この知らせが重ね描きに埋もれる)。
    let snippets = snippet_paths(&cfg);
    let (snippet_count, failed) = dict.load_snippets(&snippets);
    for (path, e) in &failed {
        eprintln!("ttyskk: {} を読めない: {e}", path.display());
    }

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
        "--- 起動 画面 {rows}x{cols} 位置の報告 {origin:?} 先走り {} バイト 定型文 {snippet_count} 語",
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
    let gate = std::sync::Arc::new(InputGate::default());
    let reader_tid = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    catch_the_wakeup_signal();
    {
        let tx = tx.clone();
        let gate = gate.clone();
        let reader_tid = reader_tid.clone();
        std::thread::spawn(move || {
            // 止めたいときに read を中断させるので、自分の居場所を知らせておく
            reader_tid.store(
                unsafe { libc::pthread_self() } as u64,
                std::sync::atomic::Ordering::Release,
            );
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 4096];
            loop {
                // 編集器を起こしている間は端末に手を出さない。**読み続けると打鍵を
                // 横取りしてしまい、編集器に何も届かない。**
                if gate.wait_while_closed() {
                    continue;
                }
                match stdin.read(&mut buf) {
                    Ok(0) => break,
                    // シグナルで中断されただけなら読み直す
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
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

    // 利用者辞書の書き換えを見張る。GUI の入力メソッドや別のペインの ttyskk が
    // 覚えたことを、起動し直さずに取り込む。
    {
        let tx = tx.clone();
        config::watch_path(user_jisyo(), move || {
            let _ = tx.send(Event::DictChanged);
        });
    }

    // 定型文の書き換えを見張る。編集器で保存した時点で使えるようにする。
    for path in &snippets {
        let tx = tx.clone();
        config::watch_path(path.clone(), move || {
            let _ = tx.send(Event::SnippetsChanged);
        });
    }

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

    // 覚えたことを書き出すまでの待ち。
    //
    // 終了時にだけ書くと、端末を開きっぱなしにしている間は他の環境へ渡らない
    // (利用者辞書を共有している場合)。かといって確定のたびに書くとディスクを
    // 叩きすぎるので、**入力が途切れてから**書く。
    const SAVE_AFTER: Duration = Duration::from_secs(3);
    let mut unsaved = false;

    loop {
        let ev = match rx.recv_timeout(SAVE_AFTER) {
            Ok(ev) => ev,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // 手が止まった。覚えたことがあれば、ここで書き出す。
                if unsaved {
                    unsaved = false;
                    if let Err(e) = skk.dict_mut().save() {
                        trace.log(format_args!("--- 利用者辞書を保存できない: {e}"));
                    }
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
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
                let mut wants_editor = None;
                // 定型文の日付を打鍵のたびに合わせる。日をまたいでも古い値が出ない。
                skk.set_now(local_now());
                for key in decoder.feed(&data) {
                    let r = skk.handle(key);
                    // 確定したなら何か覚えた見込みがある。手が止まったら書き出す。
                    unsaved |= !r.commit.is_empty();
                    to_child.extend(r.to_child());
                    // 定型文の `$0` は、渡したあとにカーソルを戻して合わせる。
                    // 子アプリの行編集に任せるので左矢印を送る (同じ行のときだけ)。
                    for _ in 0..r.cursor_back {
                        to_child.extend_from_slice(b"\x1b[D");
                    }
                    mode_changed |= r.mode_changed;
                    wants_editor = wants_editor.or(r.edit_snippet);
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
                if let Some(word) = wants_editor {
                    let tid = reader_tid.load(std::sync::atomic::Ordering::Acquire);
                    let tid = (tid != 0).then_some(tid as libc::pthread_t);
                    run_editor_over_the_screen(
                        &raw,
                        &gate,
                        tid,
                        &mut stdout,
                        &screen,
                        &word,
                        &mut trace,
                    );
                    // 書いたものをその場で使えるようにする
                    let (_, failed) = skk.dict_mut().reload_snippets();
                    for (path, e) in &failed {
                        trace.log(format_args!("--- {} を読めない: {e}", path.display()));
                    }
                    // 画面を作り直したので、重ね描きの控えは当てにならない
                    overlay = Overlay::new();
                    touched = true;
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
            Event::DictChanged => {
                // 自分が保存した直後なら、目印が一致するので読み直しは起きない
                match skk.dict_mut().reload_user() {
                    Ok(true) => trace.log(format_args!("--- 利用者辞書を読み直した")),
                    Ok(false) => {}
                    Err(e) => trace.log(format_args!("--- 利用者辞書を読み直せない: {e}")),
                }
            }
            Event::SnippetsChanged => {
                let (reloaded, failed) = skk.dict_mut().reload_snippets();
                if reloaded {
                    trace.log(format_args!("--- 定型文を読み直した"));
                }
                // 書きかけで壊れていることは珍しくない。画面を汚さず控えに残す。
                for (path, e) in &failed {
                    trace.log(format_args!("--- {} を読めない: {e}", path.display()));
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 門を閉じている間、読む係は端末に手を出さない。
    ///
    /// 閉じ忘れると編集器に打鍵が届かない (端末を読む側が二人になり奪い合う)。
    #[test]
    fn the_gate_holds_the_reader_until_opened() {
        let gate = Arc::new(InputGate::default());
        let ticks = Arc::new(AtomicUsize::new(0));
        {
            let gate = gate.clone();
            let ticks = ticks.clone();
            std::thread::spawn(move || {
                loop {
                    gate.wait_while_closed();
                    ticks.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(2));
                }
            });
        }

        std::thread::sleep(Duration::from_millis(40));
        assert!(ticks.load(Ordering::Relaxed) > 0, "開いている間は動く");

        // シグナルは送らない (試験では read で止まっていないため)
        gate.close(None);
        let at_close = ticks.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(
            ticks.load(Ordering::Relaxed),
            at_close,
            "閉じている間は止まったまま"
        );

        gate.open();
        std::thread::sleep(Duration::from_millis(40));
        assert!(ticks.load(Ordering::Relaxed) > at_close, "開ければ再び動く");
    }
}
