# Аудит: Sonatina Symphonic Orchestra

- **id**: `sonatina-symphonic-orchestra`
- **Дата проверки**: 2026-11-09
- **Метод**: реальная загрузка архива и чтение текста лицензии из первоисточника
  (`LICENSE` в репозитории `peastman/sso`; GitHub API `license.spdx_id` —
  `NOASSERTION`, поэтому лицензия фиксируется по хэшу текста).

## Источник

- **source_url**: <https://github.com/peastman/sso>
- **download.url**: <https://codeload.github.com/peastman/sso/zip/32bbdb169aef636b8216029a2e056424ba7c2abb>

## Архив

- **format**: `zip`
- **size_bytes**: 1377249014
- **sha256**: `1791e3a5f8ceb6b7e105538401ebf8133540546b986dafc76d0d94cd26f73791`

## Лицензия

- **SPDX**: `CC-Sampling+-1.0`
- **attribution_required**: `true`
- **Цитата из текста**: «Sonatina Symphonic Orchestra is a free library. It
  may be used and distributed under the terms of the Creative Commons Sampling
  Plus 1.0 license (https://creativecommons.org/licenses/sampling+/1.0/).»
  Sampling Plus разрешает копирование всего произведения только для
  некоммерческих целей (класс «Только некоммерческое»).
- **license_check**: mode `text-hash` — SHA-256 нормализованного `LICENSE`
  фиксируется и сравнивается:
  `30fc40ccc5a8a1e32e7220519b185c9a46d2c5094ff15bde64556fb7021d73b3`
  (URL `…/peastman/sso/32bbdb169…/LICENSE`).

## Сэмплы

- **Форматы**: flac (1436), wav (1626) — поддерживаются приложением.
- **Основной `.sfz`**: отсутствует в манифесте; приложение ищет `.sfz`
  обходом каталога (`find_sfz`).

## Редиректы

`download.url` на `codeload.github.com` отвечает `200` напрямую (без редиректа).
`download.redirect_hosts` пуст. SHA-256 архива гарантирует целостность.
