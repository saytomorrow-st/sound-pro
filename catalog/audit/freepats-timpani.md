# Аудит: FreePats Timpani

- **id**: `freepats-timpani`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (readme/LICENSE внутри архива и страница FreePats).

## Источник

- **source_url**: <https://github.com/freepats/timpani>
- **download.url**: <https://github.com/freepats/timpani/releases/download/2024-08-10/Timpani-SFZ%2BFLAC-20240810.7z>

## Архив

- **format**: `7z`
- **size_bytes**: 9373264
- **sha256**: `c883b0db915232b4815418e9c6b18c5b4ed73f9f7d84de3ec25785f18b8dab85`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0 Universal)
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/freepats/timpani/license` (поле `license.spdx_id`), сравнивается с `CC0-1.0`.
  `sha256` отсутствует (в режиме `metadata` сравнение по SPDX, а не
  по хэшу текста).

## Сэмплы

- **Форматы**: flac (поддерживаются приложением).
- **Основной `.sfz`**: `Timpani SFZ+FLAC-20240810/Timpani 20240809.sfz`.

## Редиректы

`download.url` на `github.com` (`…/releases/download/…`) отвечает `302` на `release-assets.githubusercontent.com`; в `download.redirect_hosts` добавлен `.githubusercontent.com`.
SHA-256 архива гарантирует целостность.
