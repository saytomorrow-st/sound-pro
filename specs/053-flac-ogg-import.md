# SPEC-053: импорт FLAC и OGG Vorbis

## Цель

Расширить существующий путь декодирования/импорта аудио (WAV и MP3) форматами
FLAC и OGG Vorbis, не дублируя его. Использовать только чистый Rust
(`symphonia` с фичами `flac` + `vorbis` + `ogg`), без системных библиотек:
AppImage и Windows zip остаются самодостаточными.

## Поведение

1. `daw_app::audio_file` декодирует FLAC и OGG Vorbis через тот же
   обобщённый путь symphonia, что и MP3 (функция `decode_mp3` становится
   `decode_with_symphonia`, принимающей расширение для `Hint`). Тип
   определяется по расширению без учёта регистра:
   - `.flac` → FLAC;
   - `.ogg`, `.oga` → OGG Vorbis;
   - `.mp3` → MP3 (как раньше);
   - `.wav` → WAV (как раньше).
2. Моно и стерео, любые частоты дискретизации (ресемплинг к частоте проекта,
   как для остальных форматов); FLAC 16 и 24 бит.
3. Расширения `.flac`, `.ogg`, `.oga` добавляются в:
   - `AUDIO_EXTENSIONS` (и, следовательно, в `is_audio_file` и фильтр drop);
   - фильтр диалога «Импорт» (`FileDialog::add_filter`).
4. Повреждённый файл даёт `EditorError::Decode` (или `EmptyFile`, если звука
   нет) — понятная ошибка в UI, без паники; проект не меняется, отмена не
   записывается.

## Ошибки

- Битый FLAC/OGG → `AudioFileError::Decode` (бывший `Mp3`, переименован в
  форматно-нейтральный `Decode`) или `AudioFileError::Empty`; на уровне
  редактора — `EditorError::Decode` / `EmptyFile`. Паники нет.
- При загрузке проекта битый FLAC/OGG → `LoadError::Decode`.

## Вне рамок

AAC/ALAC/Opus, потоковое чтение длинных файлов, точная компенсация задержки
кодера, запись (экспорт) в FLAC/OGG.

## Приёмка

- `crates/daw-app/tests/audio_file.rs`: новые тесты на декодирование FLAC и
  OGG Vorbis (длительность, каналы, частота), ошибку на мусоре, распознавание
  расширений.
- Фикстуры `crates/daw-app/tests/data/sine.flac` и `sine.ogg` (короткие,
  несколько КБ, моно/стерео), генерируются заранее и коммитятся.
- Обязательный цикл: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace --all-targets` — зелёные.
