# Сессии: клипы на таймлайне (SPEC-010)

Сессия — это модель проекта: несколько дорожек, на каждой клипы, каждый клип
— окно в свой источник. Рендер собирает дорожки как узлы графа; суммирование
делает сам `Graph`.

## Модель

`audio_core::session`:

```rust
pub struct Session {
    pub name: String,
    pub sample_rate: u32,
    pub tempo_bpm: f64,
    pub master_gain: f32, // 0.0..=1.0, SPEC-013
    pub tracks: Vec<Track>,
}

pub struct Track {
    pub name: String,
    pub gain: f32,   // 0.0..=1.0
    pub pan: f32,    // -1.0..=1.0, SPEC-013
    pub mute: bool,
    pub solo: bool,  // SPEC-013
    pub clips: Vec<Clip>,
}

pub struct Clip {
    pub start_samples: u64,   // позиция на таймлайне
    pub length_samples: u64,  // сколько длится
    pub offset_samples: u64,  // с какого сэмпла источника начинается
    pub source_length: u64,   // длина источника
    pub gain: f32,            // 0.0..=1.0
    pub name: String,
    pub source: Option<String>, // путь к WAV, SPEC-014
}
```

Ограничения: `MAX_TRACKS = 32`, `MAX_CLIPS_PER_TRACK = 128`, имена — до
`MAX_NAME_BYTES`. `Session::validate` — единственная точка проверки; ошибки
(`SessionError`) перечисляют конкретную дорожку и клип. Порядок проверок внутри
клипа: имя → `source` (пустой или длиннее `MAX_SOURCE_BYTES = 4096` —
`InvalidSource`) → gain → `length == 0` (`EmptyClip`) → `source_length == 0`
(`EmptySource`) → переполнение таймлайна (`TimelineOverflow`) → выход за
источник (`ClipOutOfSource`).

`Session::length_samples()` — максимальный `start + length` по всем дорожкам,
`0` для пустой сессии.

## JSON

Сессия хранится внутри документа проекта (SPEC-008):

```json
{
  "format": "sound-pro-project",
  "version": 1,
  "session": {
    "name": "demo",
    "sample_rate": 48000,
    "tempo_bpm": 120.0,
    "master_gain": 1.0,
    "tracks": [
      {
        "name": "low",
        "gain": 1.0,
        "pan": -0.5,
        "mute": false,
        "solo": false,
        "clips": [
          {
            "name": "440",
            "start": 0,
            "length": 24000,
            "offset": 0,
            "source_length": 24000,
            "gain": 1.0
          }
        ]
      }
    ]
  }
}
```

Порядок полей фиксирован; лишние поля и неизвестные типы отвергаются с
указанием пути (`session.tracks[0].clips[1].length`). Слишком длинный список
отвергается до декодирования элементов. `master_gain`, `pan` и `solo`
необязательны при чтении (умолчания `1`, `0`, `false`), поэтому файлы до
SPEC-013 открываются как прежде; записываются всегда.

`source` (SPEC-014) — необязательный последний ключ клипа: пишется только если
путь задан, так что документы без него сохраняются байт-в-байт как раньше.

## Файлы клипов (SPEC-014)

`daw_app::load_session_project(project, out)` читает документ и WAV каждого
клипа до начала рендера. Относительный `source` считается от каталога файла
проекта, абсолютный — как есть. Многоканальный файл сводится в моно средним;
файл с другой частотой переводится в `session.sample_rate` (SPEC-019,
`audio_core::resample`). После этого он должен быть не короче
`source_length`; лишний хвост отбрасывается. Ошибки — `LoadError`: `Read`,
`Project`, `MissingSource`, `Wav`, `SourceTooShort`, все с путём.

## Рендер

`daw_app::render_session(SessionJob)`:

- проверяет модель и соответствие `tracks.len()` числу `TrackSource`;
- монтирует по одному узлу `nodes::Track` на дорожку, `MonoClip` (или любой
  `ClipSource`) на клип;
- играет с нулевого кадра; по умолчанию длина — `Session::length_samples()`;
- пишет **стерео** float32 WAV и возвращает `SessionReport { frames, session_frames, tracks }`.

Клипы одной дорожки микшируются с clip gain в порядке `Vec`, затем
умножаются на gain дорожки; `mute` делает дорожку тишиной.

## Микшер (SPEC-013)

- **Панорама** — закон баланса: `left = pan <= 0 ? 1 : 1 - pan`,
  `right = pan >= 0 ? 1 : 1 + pan`. В центре оба множителя ровно `1`, поэтому
  сессия без панорамы в каждом канале бит-в-бит равна прежнему моно.
- **Solo**: если хоть одна дорожка в solo, звучат только solo-дорожки.
  `mute` сильнее `solo`. Решение принимает `Session::is_audible` при
  построении графа, не в `process`.
- **Мастер**: `master_gain` умножается в каждой дорожке до суммы — линейно
  то же, что после. На горячем пути нет
аллокаций, блокировок и I/O: источники создаются до старта транспорта, а
`process` только читает сэмплы по индексу.

## Эффекты (SPEC-022)

- У дорожки поле `effects` — цепочка до 8 эффектов: `eq`, `compressor`,
  `reverb`. В JSON: `{"type": "eq", "bypass": false, "low_db": 0, ...}`;
  `bypass` необязателен, ключ `effects` тоже (нет ключа — пустая цепочка).
- Цепочка стоит после моно-суммы клипов и до громкости/панорамы.
  Обойдённый эффект и плоский EQ дают сигнал бит-в-бит.
- Параметры вне диапазона — `SessionError::InvalidEffect { track, effect }`.

## Автоматизация (SPEC-023)

- У дорожки необязательный ключ `automation` с линиями `gain` (0..1) и
  `pan` (−1..1): `"automation": {"gain": [{"time": 0, "value": 0.5}]}`.
  `time` — в сэмплах, строго по возрастанию; не больше 1024 точек на линию.
- Между точками значение меняется линейно, до первой и после последней
  держится. Непустая линия заменяет статическую громкость/панораму дорожки;
  пустая автоматизация даёт звук бит-в-бит как раньше.
- Ошибки — `SessionError::InvalidAutomation { track, lane, point }`.

## Цикл (SPEC-025)

Участок цикла — состояние окна, а не проекта: в JSON не пишется, при открытии
проекта сбрасывается, отмена (Ctrl+Z) его не касается.

## CLI

```bash
cargo run -q --bin daw -- session save /tmp/demo.json   # демо-сессия
cargo run -q --bin daw -- session show /tmp/demo.json
cargo run -q --bin daw -- render-session /tmp/s.wav [--seconds 0.5]
cargo run -q --bin daw -- session render song.json /tmp/mix.wav [--seconds 2]
```

`session render` сводит проект с клипами из WAV-файлов в стерео WAV.

`render-session` рендерит встроенную демо-сессию: 440 Гц полсекунды в начале
(pan −0.5) и 660 Гц четверть секунды, начиная с четверти секунды (pan 0.5).
`session show` печатает `pan=`, `solo=` и `master_gain=`. Короче `--seconds` —
срез, длиннее — тишина в хвосте.

## Вне области

Запись аудио, drag&drop, fade/фейды кроссфейды, автоматизация клипов,
мпортаудиофайлов и выделение диапазона — не реализованы.
