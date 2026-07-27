# tanuki


<p align="center">
  <img src="assets/logo.png" alt="tanuki" width="100" />
</p>

<p align="center">
  <a href="https://tanukicode.ru">tanukicode.ru</a> · <a href="#install">install</a> · <a href="docs/next/website/src/content/docs/quick-start.mdx">quick start</a> · <a href="docs/next/website/src/content/docs">docs</a>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-666666?labelColor=333333" alt="Apache 2.0 license" /></a>
  <a href="https://github.com/BabylenMagnus/tanuki/stargazers"><img src="https://img.shields.io/github/stars/BabylenMagnus/tanuki?labelColor=333333&color=666666&logo=github" alt="GitHub stars" /></a>
</p>

---

**agent multiplexer that lives in your terminal.**

- **every agent at a glance** — blocked, working, done. real terminal views, not a wrapped interpretation.
- **detach, agents keep running** — reattach from any terminal, or over ssh. sessions survive restarts.
- **cloud relay built in** — attach to your sessions from the Tanuki web platform, from any browser, on top of your own local agent.
- **agents can use tanuki too** — a pure socket api: agents spawn panes, read output, wait on each other.
- **keyboard and mouse, both first-class** — tmux-style prefix keys *and* click, drag, split. pick per moment, not per tool.
- **one rust binary, no electron** — runs in whatever terminal you already use.

---

## install

Tanuki is not on a package manager yet — build it from source:

```bash
git clone https://github.com/BabylenMagnus/tanuki
cd tanuki
cargo build --release
```

The binary is at `target/release/tanuki`. Put it on your `PATH`, then start it where the work lives:

```bash
tanuki
```

run your agents, split panes, walk away. `ctrl+b q` detaches, `tanuki` reattaches.

Prebuilt binaries and a one-line installer will follow once release CI is wired up for this repository — see [`docs/next/website/src/content/docs/install.mdx`](docs/next/website/src/content/docs/install.mdx) for the fuller install reference (currently mid-rebrand from the upstream project).

## docs

Docs live under [`docs/next/website/src/content/docs`](docs/next/website/src/content/docs): quick start, concepts, supported agents, keyboard, configuration, session state, remote, integrations, socket api.

## agent instructions

if you are an ai agent helping with this repository, read [`AGENTS.md`](./AGENTS.md) before making changes and read [`CONTRIBUTING.md`](./CONTRIBUTING.md) before opening issues or PRs.

## development

```bash
git clone https://github.com/BabylenMagnus/tanuki
cd tanuki
cargo build --release

just test        # unit tests
just check       # formatting, tests, and maintenance checks
```

## license

Tanuki is licensed under the [Apache License 2.0](LICENSE). It is a derivative work based on [Herdr](https://github.com/ogulcancelik/herdr) — see [NOTICE](./NOTICE) for attribution and the nature of modifications.
