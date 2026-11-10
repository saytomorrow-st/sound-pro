# Аудит: FreePats Synth Strings 2

- **id**: `freepats-synth-strings-2`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (readme/LICENSE внутри архива и страница FreePats).

## Источник

- **source_url**: <https://github.com/freepats/synth-strings-2>
- **download.url**: <https://github.com/freepats/synth-strings-2/releases/download/2020-05-28/SynthStrings2-SFZ%2BFLAC-20200528.7z>

## Архив

- **format**: `7z`
- **size_bytes**: 5703244
- **sha256**: `ceec5966314d9ed7dbd0ba83850d5537096c8eab127422880dc8815766ded7c2`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0 Universal)
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/freepats/synth-strings-2/license` (поле `license.spdx_id`), сравнивается с `CC0-1.0`.
  `sha256` отсутствует (в режиме `metadata` сравнение по SPDX, а не
  по хэшу текста).

## Сэмплы

- **Форматы**: flac (поддерживаются приложением).
- **Основной `.sfz`**: `SynthStrings2 SFZ+FLAC-20200528/SynthStrings2 20200528.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/…`) отвечает `302` на `release-assets.githubusercontent.com`; в `download.redirect_hosts` добавлен `.githubusercontent.com`.
SHA-256 архива гарантирует целостность.
