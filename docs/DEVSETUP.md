# Настройка среды разработки

## 1. Инструменты

| Что | Linux (Fedora) | Windows |
|---|---|---|
| Rust (stable) | `curl https://sh.rustup.rs -sSf \| sh` | `winget install Rustlang.Rustup` |
| Компоненты | `rustup component add rustfmt clippy` | те же |
| Для realtime-бэкенда | `sudo dnf install alsa-lib-devel pkgconf-pkg-config` | ничего (WASAPI в системе) |
| Для Windows-кросс-проверки | `rustup target add x86_64-pc-windows-msvc` | — |
| Отладчик | `sudo dnf install lldb` | Visual Studio Build Tools + LLDB |
| Прослушивание результатов | любой плеер WAV | любой плеер WAV |

Проверка: `cargo --version`, `cargo clippy --version`, `cargo fmt --version`.

## 2. Редактор

Рекомендуется VS Code + расширения из `.vscode/extensions.json`
(предлагаются автоматически при открытии папки):

- **rust-analyzer** — автодополнение, переходы, диагностика (уже настроена на clippy).
- **CodeLLDB** — отладка (конфигурации в `.vscode/launch.json`).
- **Even Better TOML** — подсветка `Cargo.toml`.
- **GitHub Copilot** и **GitHub Copilot Chat** — основной агент.

Задачи: `Ctrl+Shift+B` → «check: full gate» (fmt+clippy+test), запуск
`daw selftest` и bounce — в палитре «Run Task».

Альтернативы: JetBrains RustRover + Copilot-плагин, Zed + Copilot. Все они
работают через те же CLI-команды, поэтому CI и агенты не зависят от редактора.

## 3. GitHub Copilot: что подключить в репозитории

Файлы уже созданы:

| Файл | Что делает |
|---|---|
| `AGENTS.md` | общие правила для агентов |
| `.github/copilot-instructions.md` | всегда подключаемые инструкции Copilot Chat |
| `.github/instructions/audio-core.instructions.md` | правила для `crates/audio-core/**` |
| `.github/instructions/tests.instructions.md` | правила для тестов и goldens |
| `.github/prompts/new-spec.prompt.md` | `/new-spec` — сгенерировать спецификацию |
| `.github/prompts/implement-spec.prompt.md` | `/implement-spec` — реализовать спек с тестами |
| `.github/ISSUE_TEMPLATE/agent-task.md` | тикет, который выполняет агент |

Проверка в VS Code: откройте Copilot Chat, наберите `/` и убедитесь, что
`new-spec` и `implement-spec` в списке; спросите «какие правила проекта?» —
ответ должен ссылаться на `AGENTS.md`.

Настройки в `.vscode/settings.json` включают чтение инструкций
(`github.copilot.chat.codeGeneration.useInstructionFiles`) и автоодобрение
команд `cargo fmt|clippy|test|build|check|run|metadata` (безопасные,
без сети и без `UPDATE_GOLDENS`).

## 4. Первый запуск

```bash
cargo build --workspace
cargo test --workspace --all-targets
cargo run -q --bin daw -- selftest
cargo run -q --bin daw -- bounce tone.wav 1.0   # послушать tone.wav
cargo run -q --bin daw -- bounce take.wav 30.0 --split 48000  # части по 1 с (SPEC-002)
```

Realtime-бэкенд (нужен звуковой сервер):

```bash
cargo run -p daw-app --features realtime --bin daw -- selftest
```

## 5. Git-гигиена и агенты

```bash
git init && git add -A && git commit -m "chore: project scaffold"
```

- Работайте через ветки: `task/NNN-<slug>` и PR по шаблону агентской задачи.
- Перед коммитом — полный gate (та же команда в CI).
- Никогда не коммитьте `target/` (уже в `.gitignore`) и `tone*.wav`.

## 6. Что настроить в облаке (если используете coding agent)

- Включить Copilot coding agent для репозитория; CI из
  `.github/workflows/ci.yml` уже является его проверкой.
- Добавить `.github/copilot-setup-steps.yml`, если понадобятся системные
  зависимости в агентской песочнице (например `libasound2-dev` для realtime):

```yaml
# .github/copilot-setup-steps.yml
steps:
  - name: Install ALSA headers
    if: runner.os == 'Linux'
    run: sudo apt-get update && sudo apt-get install -y libasound2-dev
```

## 7. Частые проблемы

| Симптом | Причина | Решение |
|---|---|---|
| `alsa-sys` не собирается | нет `alsa.pc` | `sudo dnf install alsa-lib-devel pkgconf-pkg-config` |
| `missing golden …wav` | нет эталона в репо | склонируйте репозиторий целиком; не запускайте `UPDATE_GOLDENS` |
| CI «goldens must match» | коммит меняет `tests/golden/` | откатите изменения эталонов |
| Тест падает на Windows, но не на Linux | расхождение `sin` | допуск `1e-6` в `golden.rs`; сравнение `to_bits()` только там, где это контракт |

## 8. Custom endpoint (OpenAI-совместимый сервер)

Расширение **GitHub Copilot LLM Gateway** уже установлено: оно направляет
запросы Copilot Chat в ваш собственный сервер.

1. `Ctrl+Shift+P` → **Configure Server** (команда
   `github.copilot.llm-gateway.manage`).
2. Указать базовый URL, например `https://llm.example.com/v1`
   (без `/chat/completions`), ключ и список моделей, если сервер его не отдаёт
   через `/models`.
3. **Test Server Connection**, затем **Refresh Models**.
4. В чате выбрать модель из пикера — она придёт с вашего сервера.
5. Если провайдер требует нестандартных заголовков — **Edit Custom Headers**;
   для моделей с маленьким контекстом — `perModelOptions` /
   `modelContextWindows`.

Агентский режим требует поддержки tool calls на сервере
(`github.copilot.llm-gateway.enableToolCalling` включён по умолчанию).
При проблемах: **Show Output Log** — там виден сырой запрос/ответ.

Держите ключ вне репозитория: настройки хранятся в профиле VS Code, но не
пишите их в `.vscode/settings.json`.

Если нужен не-Copilot клиент: Codex CLI / aider / Continue читают те же
OpenAI-совместимые эндпоинты; правила проекта у них подхватываются из
`AGENTS.md`.
