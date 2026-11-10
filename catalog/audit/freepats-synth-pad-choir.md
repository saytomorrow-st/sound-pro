# Аудит: FreePats Synth Pad Choir

- **id**: `freepats-synth-pad-choir`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (readme/LICENSE внутри архива и страница FreePats).

## Источник

- **source_url**: <https://github.com/freepats/synth-pad-choir>
- **download.url**: <https://github.com/freepats/synth-pad-choir/releases/download/2020-05-16/SynthPadChoir-SFZ%2BFLAC-20200516.7z>

## Архив

- **format**: `7z`
- **size_bytes**: 6815491
- **sha256**: `703ef30fad4861f85b319f8d549fbeeb0af0860929165c7066410dcbb03da0a3`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0 Universal)
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/freepats/synth-pad-choir/license` (поле `license.spdx_id`), сравнивается с `CC0-1.0`.
  `sha256` отсутствует (в режиме `metadata` сравнение по SPDX, а не
  по хэшу текста).

## Сэмплы

- **Форматы**: flac (поддерживаются приложением).
- **Основной `.sfz`**: `SynthPadChoir SFZ+FLAC-20200516/SynthPadChoir 20200516.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/…`) отвечает `302` на `release-assets.githubusercontent.com`; в `download.redirect_hosts` добавлен `.githubusercontent.com`.
SHA-256 архива гарантирует целостность.
