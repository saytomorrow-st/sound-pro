---
name: Задача для агента
about: Тикет, который может выполнить ИИ-агент без уточнений
title: "[agent] "
labels: agent
---

## Цель

<одно предложение: какое наблюдаемое поведение должно появиться>

## Спецификация

specs/NNN-название.md

## Контекст

- Затрагиваемые крейты/модули: <...>
- Связанные задачи/PR: <...>

## Файлы, которые можно менять

- [ ] crates/audio-core/src/___
- [ ] crates/audio-core/tests/___

## Критерии приёмки

- [ ] Поведение соответствует спецификации
- [ ] Добавлены юнит-тесты на каждое утверждение спецификации
- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --all-targets` — зелёные
- [ ] `<поведенческий критерий, например: bounce 1 с даёт ровно 48000 сэмплов>`

## Запрещено

- Менять `crates/audio-core/tests/golden/*.wav` и запускать `UPDATE_GOLDENS=1`
- Ослаблять существующие тесты
- Добавлять `unsafe` в `audio-core`
- Аллокации и блокировки в аудиопотоке (см. `docs/REALTIME.md`)

## Как проверить

```bash
cargo test --workspace --all-targets
cargo run -q --bin daw -- selftest
```
