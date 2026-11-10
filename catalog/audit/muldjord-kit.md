# Аудит: Muldjord Kit

- **id**: `muldjord-kit`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/DrumGizmo.MuldjordKit`; GitHub API
  `license.spdx_id` привязан к ветке по умолчанию).

## Источник

- **source_url**: <https://github.com/sfzinstruments/DrumGizmo.MuldjordKit>
- **download.url**: <https://codeload.github.com/sfzinstruments/DrumGizmo.MuldjordKit/zip/fc165714974aa843125ce88597f341b31376e1d5>

## Архив

- **format**: `zip`
- **size_bytes**: 345968791
- **sha256**: `90a1e2d8c749a67d84a7e7277217e45d704ba1d0297c7b13d4772fa4825397be`

## Лицензия

- **SPDX**: `CC-BY-4.0`
- **attribution_required**: `true`
- **Первая строка текста лицензии**: «Creative Commons Attribution 4.0
  International» (CC BY 4.0, <https://creativecommons.org/licenses/by/4.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/DrumGizmo.MuldjordKit/license`
  (поле `license.spdx_id`), сравнивается с `CC-BY-4.0`. `sha256` отсутствует.

## Сэмплы

- **Форматы**: flac (4064) — поддерживаются приложением.
- **Основной `.sfz`**: `DrumGizmo.MuldjordKit-master/DrumGizmo/MuldjordKit/Stereo/DrumGizmo MuldjordKit.sfz`.

## Редиректы

`download.url` на `codeload.github.com` отвечает `200` напрямую (без редиректа).
`download.redirect_hosts` пуст. SHA-256 архива гарантирует целостность.
