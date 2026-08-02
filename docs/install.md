# インストールの細かいところ

README のインストールで足りるのは、crates.io から入れる場合です。ここでは開発版を追う場合
と、入れ替わったかを確かめる方法を説明します。

## 公開した版より先を追う

GitHub から直接入れられます。

```sh
cargo install --locked --git https://github.com/barewalker/ttyskk
```

`--locked` を付けると、同梱の `Cargo.lock` がそのまま使われます。付けない場合、cargo は
依存をその時点の最新で解決し直すため、依存の側が新しい Rust を要求していると止まることが
あります。

手元にクローンしてあるなら `cargo install --path .` で足ります。

## 入れ替わったかを確かめる

版番号だけでは分からないことがあるので、組み立てた時点のコミットを添えてあります。

```console
$ ttyskk --version
ttyskk 0.2.0 (8dc4b375)
```

末尾の `+` は、手を入れたまま組み立てた印です。crates.io から入れた場合は、公開した時点の
コミットが出ます。

## 更新が古いまま入るとき

cargo は git の写しを溜め込むので、`--git` からの更新が**古い写しのまま入る**ことがあり
ます。上の `--version` でコミットが変わっていなければ、版を名指しするか、写しを捨ててから
入れ直してください。

```sh
cargo install --force --git https://github.com/barewalker/ttyskk --rev <コミット>
rm -rf ~/.cargo/git/db/ttyskk-* ~/.cargo/git/checkouts/ttyskk-*
```

## 動く環境

依存するのは POSIX の擬似端末と、VT100 系のエスケープ列を解する端末だけです。tmux /
screen / SSH / mosh のどれとも組み合わせられますし、何も挟まなくても動きます。

Linux と macOS で動きます。WSL2 でも動きますが、**Windows そのものでは動きません**
(termios と POSIX 擬似端末を使っているためです)。
