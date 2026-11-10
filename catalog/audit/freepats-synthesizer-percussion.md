# Аудит: FreePats Synthesizer Percussion

- **id**: `freepats-synthesizer-percussion`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (readme/LICENSE внутри архива и страница FreePats).

## Источник

- **source_url**: <https://github.com/freepats/synthesizer-percussion>
- **download.url**: <https://freepats.zenvoid.org/Percussion/SynthesizerPercussion/SynthesizerPercussion-SFZ-20220718.7z>

## Архив

- **format**: `7z`
- **size_bytes**: 1853650
- **sha256**: `dbb2e5bb8268022fffa6dcc3d11a93368316038bf3ae81a965c58e9d490ed23b`

## Лицензия

- **SPDX**: `CC0-1.0`
- **attribution_required**: `false`
- **Первая строка текста лицензии**: «Creative Commons Legal Code» (CC0 1.0 Universal)
- **license_check**: mode `metadata` — SPDX получается из
  `https://api.github.com/repos/freepats/synthesizer-percussion/license` (поле `license.spdx_id`), сравнивается с `CC0-1.0`.
  `sha256` отсутствует (в режиме `metadata` сравнение по SPDX, а не
  по хэшу текста).

## Сэмплы

- **Форматы**: wav (поддерживаются приложением).
- **Основной `.sfz`**: `SynthesizerPercussion-SFZ-20220718/SynthesizerPercussion-20220718.sfz`.

## Редиректы

`download.url` отвечает `200` напрямую (без редиректа); `download.redirect_hosts` пуст.
SHA-256 архива гарантирует целостность.
