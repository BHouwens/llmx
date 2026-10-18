# llmx

A launcher that puts a model picker in front of [codex-local], so using a big
local or remote model for agentic coding is one command instead of six.

```
cd ~/code/some-project
llmx
```

Pick a model, press Enter, and you are inside codex-local talking to it.

---

## What it solves

Using a large model with codex-local normally means doing all of this by hand,
differently for every model:

- start a local inference server (MLX, OMLX, vLLM, SGLang, …)
- or open an SSH tunnel to a rented GPU and remember which port you forwarded
- work out which model id the server is actually serving
- write a codex-local provider block with the right `base_url`, `wire_api`,
  context window and output limit
- launch codex-local with that configuration
- afterwards, remember to close the tunnel and stop the server

llmx does all of it from a declarative profile, and undoes the parts it did
when codex-local exits.

**llmx performs no inference.** It is infrastructure around codex-local: it
resolves a profile into a healthy OpenAI-compatible endpoint, hands codex-local
the matching configuration, and gets out of the way.

> **codex-local is not upstream `codex`.** They are separate programs with
> separate homes (`~/.codex-local` and `~/.codex`). llmx targets codex-local
> and never reads or writes `~/.codex`.

---

## Architecture

The repository and the agent stay on your machine. A remote GPU receives
inference requests and nothing else — no source code is ever copied to it.

```
   your machine                                  remote host (optional)
 ┌──────────────────────────────────┐          ┌─────────────────────────┐
 │  local repository                │          │                         │
 │        │                         │          │                         │
 │        ▼                         │          │                         │
 │  codex-local  ◄── config ── llmx │          │                         │
 │        │                    │    │          │                         │
 │        ▼                    │    │          │                         │
 │  127.0.0.1:8100/v1          │    │          │                         │
 │        │                    │    │          │                         │
 │        │            spawns ─┘    │          │                         │
 │        ▼                         │          │                         │
 │  ssh -N -L 8100:127.0.0.1:8000 ──┼──────────┼──► 127.0.0.1:8000       │
 │                                  │          │          │              │
 └──────────────────────────────────┘          │          ▼              │
                                               │   inference server      │
                                               │   (vLLM / SGLang / …)   │
                                               │          │              │
                                               │          ▼              │
                                               │      large LLM          │
                                               └─────────────────────────┘
```

For a local model the same picture holds with the tunnel removed: llmx starts
the server directly and codex-local talks to `127.0.0.1`.

All three transports produce the same thing — a base URL that answers — so
codex-local never learns whether it is talking to MLX, vLLM, a tunnel, or a
machine on your LAN.

---

## Installation

Requires Rust 1.82 or newer, and codex-local already installed.

```bash
git clone <this-repo> llmx
cd llmx
cargo build --release
install -m 755 target/release/llmx ~/.local/bin/llmx    # or anywhere on PATH
```

When building from this checkout, the same steps are available as:

```bash
./scripts/install.sh                    # installs to ~/.local/bin/llmx
./scripts/install.sh /some/bin/directory # optional install directory
```

The result is one native binary of about 4 MB with no runtime dependencies.

Check the setup:

```bash
llmx doctor
```

---

## Initial configuration

llmx reads `~/.config/llmx/models.toml`. Override the location with
`LLMX_CONFIG=/path/to/models.toml`.

```bash
mkdir -p ~/.config/llmx
cp examples/models.toml ~/.config/llmx/models.toml
$EDITOR ~/.config/llmx/models.toml
```

`examples/models.toml` covers every backend and is commented throughout. The
model names, ports and flags in it are **examples** — check them against the
runtime you actually have installed.

A minimal profile:

```toml
[[models]]
id = "my-model"                    # used by `llmx run my-model`
name = "My Model"                  # shown in the picker
backend = "local"                  # local | ssh | remote
context_window = 131072
max_output_tokens = 32768
# Optional PNG/JPEG shown in the picker details; relative to this config file.
# logo = "logos/my-model.png"

[models.endpoint]
url = "http://127.0.0.1:8000/v1"   # OpenAI-compatible base URL
```

### Every field

| Field | Applies to | Meaning |
|---|---|---|
| `id` | all | Stable key. Used by `llmx run <id>` and as a filename, so letters, digits, `-`, `_`, `.` only. |
| `name` | all | Display name in the picker. |
| `model_id` | all | Model id the server expects. **Omit it** to discover from `GET /v1/models`. |
| `backend` | all | `local`, `ssh` or `remote`. |
| `label` | all | Short tag in the picker's second column. Defaults to the backend name. |
| `logo` | all | Optional PNG/JPEG image shown for the selected model. Relative paths are resolved from the `models.toml` directory. Terminals without image graphics use half-block rendering. |
| `context_window` | all | Tokens in. Passed to codex-local as `model_context_window`. |
| `max_output_tokens` | all | Tokens out. Must be smaller than `context_window`. |
| `auto_compact_token_limit` | all | When codex-local compacts history. |
| `wire_api` | all | `chat` (default) or `responses`. |
| `endpoint.url` | all | Base URL ending in `/v1`, not the `/chat/completions` path. |
| `endpoint.env_key` | all | *Name* of an environment variable holding an API key. llmx never stores key material. |
| `runtime.command` / `.args` / `.env` / `.cwd` | local | How to start the server, if llmx should. |
| `runtime.ready_timeout_secs` | local | Override the global health timeout. Large models need minutes. |
| `ssh.host` | ssh | A `Host` alias from `~/.ssh/config`, or `user@hostname`. |
| `ssh.local_port` | ssh | Port bound here. Must match the port in `endpoint.url`. |
| `ssh.remote_host` / `.remote_port` | ssh | Where the server listens *on the remote machine*. Defaults to `127.0.0.1`. |
| `ssh.extra_args` | ssh | Extra `ssh` flags, spliced in before the host. |
| `resources.*` | all | Advisory RAM and VRAM hints for the picker. RAM hints drive the existing fit warning; they never block a launch. |
| `[models.info]` | all | Optional `provider`, `quantization`, `pruning`, and `hardware` labels for the picker. |
| `resources.*_vram_gb` | all | Optional minimum, recommended, or estimated GPU memory in GB. |
| `[models.codex]` | all | Arbitrary keys merged into the generated codex-local config. |

---

## Example: a local MLX model

Install the MLX runtime once (Apple silicon only):

```bash
uv tool install --upgrade mlx-lm
hf download mlx-community/Qwen3.8-27B-4bit
```

Then add this profile to `~/.config/llmx/models.toml`:

```toml
[[models]]
id = "qwen38-27b"
name = "Qwen3.8 27B (4-bit)"
model_id = "mlx-community/Qwen3.8-27B-4bit"
backend = "local"
label = "MLX"
context_window = 131072
max_output_tokens = 32768

[models.endpoint]
url = "http://127.0.0.1:8000/v1"

[models.runtime]
command = "mlx_lm.server"
args = [
  "--model", "mlx-community/Qwen3.8-27B-4bit",
  "--host", "127.0.0.1",
  "--port", "8000",
]
ready_timeout_secs = 600

[models.resources]
estimated_ram_gb = 22
recommended_ram_gb = 32

[models.codex]
# Uses a string `cmd`, matching Qwen's tool-call output.
experimental_use_exec_command_tool = true
```

On launch llmx checks whether `http://127.0.0.1:8000/v1` already answers.

- **It answers** → llmx uses it and will **not** stop it afterwards. Someone
  else started it; it is not llmx's to kill.
- **It does not** → llmx starts `mlx_lm.server`, waits for the endpoint, and
  stops it when codex-local exits.

Current versions of `mlx_lm.server` list every compatible model in the local
Hugging Face cache from `GET /v1/models`, not just the model named by
`--model`. Pre-downloading the weights makes the model visible to that endpoint;
setting `model_id` prevents another cached MLX model from being selected.

Check the wiring without downloading or starting the model:

```bash
llmx doctor
llmx explain qwen38-27b
```

To test-drive it against another repository, change to that repository and
launch the profile. llmx passes that working directory to codex-local, so the
agent can inspect the repository from the terminal:

```bash
cd ~/code/project-to-inspect
llmx run qwen38-27b
```

The `hf download` step downloads about 16 GB of model weights into the Hugging
Face cache and can take a while. At the codex-local prompt, a useful read-only
first task is:

```text
Inspect this repository without changing files. Explain its architecture,
entry points, test commands, and the three highest-risk areas.
```

Exit codex-local normally when finished; llmx then stops the MLX server it
started. If the endpoint was already running, llmx leaves it running.

---

## Example: SSH to a rented GPU

```toml
[[models]]
id = "deepseek-v4-spark"
name = "DeepSeek V4 Flash — DGX Spark"
backend = "ssh"
label = "SPARK"
context_window = 262144
max_output_tokens = 32768

[models.endpoint]
url = "http://127.0.0.1:8100/v1"   # must match ssh.local_port

[models.ssh]
host = "spark"
local_port = 8100
remote_host = "127.0.0.1"
remote_port = 8000
extra_args = ["-o", "ServerAliveInterval=30"]
```

llmx runs exactly:

```bash
ssh -N -o ExitOnForwardFailure=yes -L 8100:127.0.0.1:8000 spark
```

- `-N` — open the forward, run no remote command.
- `ExitOnForwardFailure=yes` — if the local port is taken, ssh exits with an
  error instead of sitting there doing nothing. llmx notices within a second
  and shows you ssh's own message.
- `remote_host = "127.0.0.1"` — the model server stays on the remote machine's
  loopback interface. **Never expose the remote API port publicly.**

`llmx explain deepseek-v4-spark` prints the exact command without running it.

### Using SSH aliases

`ssh.host` is passed straight to OpenSSH, so your existing `~/.ssh/config` does
all the work — identity files, jump hosts, ports, usernames:

```sshconfig
Host spark
    HostName 203.0.113.42
    User ubuntu
    IdentityFile ~/.ssh/spark_ed25519
    Port 22

Host h200-box
    HostName vast-instance.example.com
    User root
    Port 41022
    ProxyJump bastion
```

Then `host = "spark"` is all the profile needs. llmx deliberately implements no
credential handling of its own: **no passwords are stored, read or prompted for.**
If your key needs a passphrase, use `ssh-agent` as you normally would.

`llmx doctor` checks each alias offline with `ssh -G`, and tells you when a
host is not actually defined in your config (it would then be used as a literal
hostname).

### Cloud-provider independence

A cloud machine is just an SSH target exposing an OpenAI-compatible server.
The same profile shape works for a rented DGX Spark, RunPod, Vast.ai, your own
server in a cupboard, or a box on your LAN. llmx has no provider integrations
and no provider-specific code.

For a machine reachable directly, skip SSH entirely:

```toml
[[models]]
id = "lan-box"
name = "Workshop Server"
backend = "remote"

[models.endpoint]
url = "http://192.168.1.50:8000/v1"
```

---

## What happens when you press Enter

1. **Resolve the profile** from `models.toml`.
2. **Check the endpoint.** If it already answers, llmx uses it and records that
   it does not own it.
3. **Start the backend** if needed — spawn the runtime, or open the tunnel.
4. **Wait for health**, polling `GET <url>/models` with exponential backoff,
   showing elapsed time, attempt count and the most recent error. Cancel any
   time with `q` or Ctrl-C.
5. **Resolve the model id.** Uses `model_id` when set and served. Discovers it
   when the server serves exactly one. **Asks you** when there are several —
   llmx will not pick silently, because that produces a session that looks
   correct while running the wrong weights.
6. **Generate the codex-local config** and write it to
   `~/.local/share/llmx/generated/<id>.toml`.
7. **Launch codex-local** in your current directory and give it the terminal.
8. **Clean up** whatever llmx started, once codex-local exits.

### The generated configuration

```toml
model = "deepseek-v4-flash"
model_provider = "llmx-deepseek-v4-spark"
model_context_window = 262144
model_max_output_tokens = 32768

[model_providers.llmx-deepseek-v4-spark]
name = "llmx: DeepSeek V4 Flash — DGX Spark"
base_url = "http://127.0.0.1:8100/v1"
wire_api = "chat"
request_max_retries = 2
stream_max_retries = 2
stream_idle_timeout_ms = 300000
requires_openai_auth = false
```

Every key was checked against the codex-local source, not its README:
`ConfigToml` in `codex-rs/core/src/config.rs` and `ModelProviderInfo` in
`codex-rs/core/src/model_provider_info.rs`. `requires_openai_auth = false` is
what stops codex-local showing its OpenAI login screen for a provider that has
no OpenAI account behind it.

**Your `~/.codex-local/config.toml` is never modified.** By default llmx passes
the generated keys as `-c key=value` overrides, which layer on top of your own
config — so your MCP servers, session history and personal settings keep
working. The file on disk is written anyway, so you can read exactly what was
used when something goes wrong.

If you want a completely isolated session instead:

```toml
[settings]
isolation = "codex_home"
```

This points `CODEX_HOME` at a directory llmx owns. Fully isolated — and that
includes your MCP servers and history, which will not load.

Provider entries are named `llmx-<profile id>` so they can never collide with a
provider you defined yourself.

---

## Cleanup

llmx tracks exactly what it started, and only touches that.

| Situation | What llmx does on exit |
|---|---|
| llmx started the model server | Stops it. SIGTERM to its process group, SIGKILL after 5s. |
| llmx opened the SSH tunnel | Closes it, same way. |
| The endpoint was **already running** | Leaves it completely alone. |
| `backend = "remote"` | Nothing to clean up; llmx never starts anything. |

Backend processes are spawned into their own process group, so a launcher
script that forks the real server is cleaned up along with its children rather
than leaving the port held.

This holds whether codex-local exits normally, crashes, or you Ctrl-C out of
the startup screen. If the tunnel or the server dies *before* the endpoint comes
up, llmx stops waiting immediately and shows you what the process printed.

codex-local itself is deliberately **not** put in a separate process group: it
has to stay in the foreground group to own the terminal.

---

## Commands

```
llmx                  open the model picker (default)
llmx run <id>         launch a profile directly, no picker
llmx resume <id>      resume a conversation with a profile (opens session picker)
llmx resume <id> <uuid>  resume a specific conversation
llmx list             list configured profiles
llmx status           probe every endpoint; starts nothing
llmx doctor           check config, executables, ssh aliases, writable paths
llmx explain <id>     show the tunnel, the generated config and the exact
                      command line, without running anything
llmx logs             print the newest session log  (--list, -n N)
```

`llmx resume` starts the selected profile's backend and launches
`codex-local resume` with that profile's generated model configuration. With
no UUID, codex-local shows its conversation picker; with a UUID, it resumes
that conversation directly.

### Keys in the picker

| Key | Action |
|---|---|
| `↑` `↓` or `k` `j` | select |
| `Enter` | launch |
| `r` | re-probe every endpoint |
| `l` | toggle the log pane |
| `q` / `Esc` | quit |

---

## Logging

Session logs are written to `~/.local/state/llmx/logs/<timestamp>-<profile>.log`
and capture llmx's own events, the model server's stdout and stderr, ssh's
stderr, and codex-local's exit status. `llmx logs` prints the newest one; `l` in
the picker tails it live.

API keys are never logged — profiles name an environment *variable*, and llmx
forwards only the name.

---

## Troubleshooting

**`endpoint never became healthy` / timeout**

Check the endpoint by hand: `curl http://127.0.0.1:8100/v1/models`. Then
`llmx logs` — the model server's own stderr is in there, and it usually says
exactly what went wrong (wrong model path, out of memory, port in use). Raise
`runtime.ready_timeout_secs` if a large model is simply slow to load.

**`ssh tunnel (host) exited before the endpoint became ready`**

ssh's own message follows on the next lines. The common causes are: the local
port is already in use (change `ssh.local_port`, and `endpoint.url` to match),
the host alias does not resolve (`llmx doctor`), or your key needs a passphrase
and no agent is running (`ssh-add ~/.ssh/your_key`).

**`serves several models and the profile sets no model_id`**

The endpoint offers more than one model. Either add `model_id = "..."` to the
profile, or run `llmx` interactively and choose from the list. llmx will not
guess.

**`the profile asks for model X, but the endpoint serves: Y, Z`**

`model_id` is stale. `curl <url>/models` to see what is really loaded.

**codex-local asks about trusting the directory**

That is codex-local's own first-run prompt, not llmx. Answer it once, or set an
approval policy in the profile to skip it:

```toml
[models.codex]
approval_policy = "on-request"
```

**codex-local shows a login screen**

It should not — llmx sets `requires_openai_auth = false`. If it happens, check
`llmx explain <id>` to confirm that key is in the generated config.

**A model server is still running after llmx exits**

Then llmx did not start it. llmx only stops processes it spawned itself; a
server you started in another terminal is yours to stop. `llmx status` shows
what is currently listening.

**Memory warning in the picker**

Advisory only, based on the `[models.resources]` numbers *you* wrote, compared
against total physical RAM. llmx does not try to compute available unified GPU
memory, and never blocks a launch on this.

---

## Development

```bash
cargo test
cargo clippy --all-targets
cargo fmt
```

Tests use a small in-process HTTP server and dummy child processes, so nothing
reaches a real model server or a remote machine.

Layout:

```
src/
  main.rs         CLI parsing and Tokio runtime
  app.rs          profile launch lifecycle and cleanup
  app/commands.rs list, status, doctor, logs and explain commands
  config/         TOML schema, loading and validation
  backend/        local, SSH and direct endpoints behind `Backend`
  endpoint/       OpenAI-compatible health checks and model discovery
  codex/          generated config, delivery strategy and agent process
  process/        owned child processes, output capture and cleanup
  tui/            picker/startup state, events and rendering
  logging/        per-session logs and in-memory log tail
  paths.rs        config, state and data locations
  system.rs       physical RAM detection for advisory warnings
```

The launch flow is `main` → `App::run_profile` or `App::run_tui` → backend
startup and endpoint health check → model id resolution → codex-local config
generation → agent launch → backend cleanup. `Backend` isolates transport and
process ownership: local, SSH and direct endpoints all return the same served
model list. Endpoint discovery and config generation do not depend on which
transport produced it.

Configuration structs mirror the TOML file. `config::loader` parses the file
and `config::validation` checks cross-field rules before `App` can use it. The
TUI owns presentation state and input; it delegates backend work through the
same trait used by `llmx run`. `ProcessManager` only tracks children spawned by
llmx, which keeps cleanup from signaling a server that was already running.

macOS is the primary platform; Linux is supported. Windows is not.

[codex-local]: https://github.com/0xSero/codex-local
