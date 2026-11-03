# Аудит: Salamander Grand Piano V3

- **id**: `salamander-grand-v3`
- **Дата проверки**: 2026-11-03 (SPEC-067, PR 6)
- **Метод**: реальная загрузка архива и текста лицензии с апстрима.

## Источник

- **source_url**: <https://archive.org/details/SalamanderGrandPianoV3>
- **download.url**: <https://archive.org/download/SalamanderGrandPianoV3/SalamanderGrandPianoV3_44.1khz16bit.tar.bz2>

## Архив

- **format**: `tar.bz2` (НЕ zip/flac, как предполагалось в RFC-001 §6)
- **size_bytes**: 488713261
- **sha256**: `fe595c7722b70860e6377f82948d1c0cfcf27ecebf500dc79534408a78a62892`

## Лицензия

- **SPDX**: `CC-BY-3.0`
- **attribution_required**: `true`
- **license_check**: mode `text-hash` — загружаем
  `https://creativecommons.org/licenses/by/3.0/legalcode.txt`, нормализуем
  (UTF-8 lossy, trim, CRLF→LF), считаем SHA-256 и сравниваем с закреплённым
  значением. Файл: **19467 байт**. Хэш:
  `11dbf7126ae9794e4a7dc54334f3c82a67efcf22df888d396dc2ed707b2a3f9b`.

## Редиректы

`download.url` на `archive.org` отвечает `302` на
`https://dn*.ca.archive.org/0/items/…`. В `download.redirect_hosts` добавлен
`.ca.archive.org` (HTTPS-only, суффикс — любой поддомен `dn*`). SHA-256
архива по-прежнему гарантирует целостность, независимо от CDN-хоста.

## Замечания для интеграции

SFZ внутри архива использует backslash `\` в качестве разделителя путей
(Windows-стиль). Для нашего подмножества требуется приведение `\` → `/`.
Это задокументировано в PR как необходимое преобразование, а не внесено
молча. Формат архива `tar.bz2` (не zip) — добавлены зависимости `tar` и
`bzip2`.
