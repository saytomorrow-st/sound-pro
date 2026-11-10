# Аудит: Ixox Flute

- **id**: `ixox-flute`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `sfzinstruments/Ixox.Flute`; GitHub API
  `license.spdx_id` привязан к ветке по умолчанию).

## Источник

- **source_url**: <https://github.com/sfzinstruments/Ixox.Flute>
- **download.url**: <https://codeload.github.com/sfzinstruments/Ixox.Flute/zip/0cc54468bb0d2d9b32921958585caad65ba8df21>

## Архив

- **format**: `zip`
- **size_bytes**: 9526829
- **sha256**: `e5fb9116de784e1635e5fe842a6285745d6949424037dffa8246d542ad921079`

## Лицензия

- **SPDX**: `CC-BY-4.0`
- **attribution_required**: `true`
- **Первая строка текста лицензии**: «Creative Commons Attribution 4.0
  International» (CC BY 4.0, <https://creativecommons.org/licenses/by/4.0/legalcode>).
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/sfzinstruments/Ixox.Flute/license` (поле
  `license.spdx_id`), сравнивается с `CC-BY-4.0`. `sha256` отсутствует.

## Сэмплы

- **Форматы**: flac (81) — поддерживаются приложением.
- **Основной `.sfz`**: `Ixox.Flute-master/Ixox Flute.sfz`.

## Редиректы

`download.url` на `codeload.github.com` отвечает `200` напрямую (без редиректа).
`download.redirect_hosts` пуст. SHA-256 архива гарантирует целостность.
