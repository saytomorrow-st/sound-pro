# Аудит: FreePats Synth Strings 1

- **id**: `freepats-synth-strings-1`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (readme/LICENSE внутри архива и страница FreePats).

## Источник

- **source_url**: <https://github.com/freepats/synth-strings-1>
- **download.url**: <https://github.com/freepats/synth-strings-1/releases/download/2020-05-28/SynthStrings1-SFZ%2BWAV-20200528.7z>

## Архив

- **format**: `7z`
- **size_bytes**: 3380050
- **sha256**: `912709514f5dc4ae5f50e07e5b7141133afe15869165dea7551604ff998cee2c`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0 Universal)
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/freepats/synth-strings-1/license` (поле `license.spdx_id`), сравнивается с `CC0-1.0`.
  `sha256` отсутствует (в режиме `metadata` сравнение по SPDX, а не
  по хэшу текста).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `SynthStrings1 SFZ+WAV-20200528/SynthStrings1 20200528.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/…`) отвечает `302` на `release-assets.githubusercontent.com`; в `download.redirect_hosts` добавлен `.githubusercontent.com`.
SHA-256 архива гарантирует целостность.
