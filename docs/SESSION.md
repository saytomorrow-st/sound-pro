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
    pub tracks: Vec<Track>,
}

pub struct Track {
    pub name: String,
    pub gain: f32,   // 0.0..=1.0
    pub mute: bool,
    pub clips: Vec<Clip>,
}

pub struct Clip {
    pub start_samples: u64,   // позиция на таймлайне
    pub length_samples: u64,  // сколько длится
    pub offset_samples: u64,  // с какого сэмпла источника начинается
    pub source_length: u64,   // длина источника
    pub gain: f32,            // 0.0..=1.0
    pub name: String,
}
```

Ограничения: `MAX_TRACKS = 32`, `MAX_CLIPS_PER_TRACK = 128`, имена — до
`MAX_NAME_BYTES`. `Session::validate` — единственная точка проверки; ошибки
(`SessionError`) перечисляют конкретную дорожку и клип. Порядок проверок внутри
клипа: имя → gain → `length == 0` (`EmptyClip`) → `source_length == 0`
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
    "tracks": [
      {
        "name": "low",
        "gain": 1.0,
        "mute": false,
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
отвергается до декодирования элементов.

## Рендер

`daw_app::render_session(SessionJob)`:

- проверяет модель и соответствие `tracks.len()` числу `TrackSource`;
- монтирует по одному узлу `nodes::Track` на дорожку, `MonoClip` (или любой
  `ClipSource`) на клип;
- играет с нулевого кадра; по умолчанию длина — `Session::length_samples()`;
- пишет mono float32 WAV и возвращает `SessionReport { frames, session_frames, tracks }`.

Клипы одной дорожки микшируются с clip gain в порядке `Vec`, затем
умножаются на gain дорожки; `mute` делает дорожку тишиной. На горячем пути нет
аллокаций, блокировок и I/O: источники создаются до старта транспорта, а
`process` только читает сэмплы по индексу.

## CLI

```bash
cargo run -q --bin daw -- session save /tmp/demo.json   # демо-сессия
cargo run -q --bin daw -- session show /tmp/demo.json
cargo run -q --bin daw -- render-session /tmp/s.wav [--seconds 0.5]
```

`render-session` рендерит встроенную демо-сессию: 440 Гц полсекунды в начале и
660 Гц четверть секунды, начиная с четверти секунды. Короче `--seconds` —
срез, длиннее — тишина в хвосте.

## Вне области

Запись аудио, drag&drop, fade/фейды кроссфейды, автоматизация клипов,
мпортаудиофайлов и выделение диапазона — не реализованы.
