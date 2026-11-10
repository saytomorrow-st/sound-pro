# Аудит: G-Town Church Sampling Project

- **id**: `gtownchurch`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (README в репозитории `sfzinstruments/GTownChurchSamplingProject`; GitHub API
  `license.spdx_id` не задан, поэтому лицензия фиксируется по хэшу текста).

## Источник

- **source_url**: <https://github.com/sfzinstruments/GTownChurchSamplingProject>
- **download.url**: <https://codeload.github.com/sfzinstruments/GTownChurchSamplingProject/zip/6a1454ad43c24a91b1a55ae053dd8b0e148969e4>

## Архив

- **format**: `zip`
- **size_bytes**: 457439813
- **sha256**: `6347d07cd88ddb2f0119e546d2d3b66d9b7e4c7043ac3bc79a553124589b57b0`

## Лицензия

- **SPDX**: `CC-Sampling+-1.0`
- **attribution_required**: `true`
- **Цитата из текста**: «The G-Town Church Sampling Project is licensed under
  the Creative Commons Sampling Plus 1.0 license:
  <https://creativecommons.org/licenses/sampling+/1.0/>». Распространение
  всего произведения — только некоммерческое (класс «Только некоммерческое»).
- **license_check**: mode `text-hash` — SHA-256 нормализованного README
  фиксируется и сравнивается:
  `89bf8bd7798298555a3aeaa5189e1d6b926d0409272751a2e0f886144a83ad29`
  (URL `…/GTownChurchSamplingProject/6a1454ad…/README.md`).

## Сэмплы

- **Форматы**: wav (1048) — поддерживаются приложением.
- **Основной `.sfz`**: отсутствует в манифесте; приложение ищет `.sfz`
  обходом каталога (`find_sfz`).

## Редиректы

`download.url` на `codeload.github.com` отвечает `200` напрямую (без редиректа).
`download.redirect_hosts` пуст. SHA-256 архива гарантирует целостность.
