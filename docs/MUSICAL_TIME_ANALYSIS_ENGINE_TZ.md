# Техническое задание

## Musical Time Analysis Engine

### 1. Цель

Разработать production-quality модуль анализа музыкального времени для DAW, ориентированного на работу с сэмплингом.

Модуль должен анализировать загружанный аудиофайл и определять:

* глобальный BPM;
* альтернативные BPM-гипотезы;
* beat grid;
* фазу первого beat;
* downbeats / bars, если возможно;
* локальный tempo;
* стабильность темпа;
* rhythmicity;
* confidence каждого результата;
* возможность построения warp map.

Главная архитектурная идея:

> Не пытаться напрямую определить BPM. Сначала извлекать ритмическую информацию из аудио, затем генерировать tempo hypotheses, затем восстанавливать beat grid и только после этого выбирать наиболее вероятную метрическую интерпретацию.

Результатом должен быть не одно число BPM, а структурированное описание музыкального времени.

---

# 2. Основные требования

Система должна:

1. Работать с короткими samples и длинными музыкальными треками.
2. Корректно обрабатывать half-time / double-time ambiguity.
3. Работать с синкопированным ритмом.
4. Работать с разной интенсивностью transient'ов.
5. Поддерживать tempo drift / rubato.
6. Не назначать произвольный BPM неритмическому материалу.
7. Возвращать confidence.
8. Возвращать несколько tempo candidates, если существует существенная неоднозначность.
9. Отделять понятия:

   * BPM;
   * beat positions;
   * downbeats;
   * tempo trajectory.
10. Позволять использовать результаты для последующего:

* time stretching;
* warp;
* slicing;
* loop detection;
* quantization;
* sample synchronization.

---

# 3. Главный API

Предусмотреть API уровня:

```text
analyzeAudio(audio) -> MusicalTimeAnalysis
```

Результат концептуально:

```text
MusicalTimeAnalysis {
    duration

    tempo {
        bpm
        confidence
        alternatives[]
        stability
    }

    beats[]
    downbeats[]
    bars[]

    tempoCurve[]

    rhythmicity
    meter

    warpMap

    diagnostics
}
```

Конкретные типы и имена должны соответствовать существующей архитектуре проекта.

---

# 4. Архитектура pipeline

Реализовать pipeline:

```text
Audio
  ↓
Preprocessing
  ↓
Multi-resolution / multi-band feature extraction
  ↓
Onset representation
  ↓
Rhythmicity estimation
  ↓
Tempogram / periodicity analysis
  ↓
Tempo hypothesis generation
  ↓
Beat tracking
  ↓
Phase optimization
  ↓
Half/double-time disambiguation
  ↓
Downbeat / meter analysis
  ↓
Local tempo analysis
  ↓
Confidence estimation
  ↓
MusicalTimeAnalysis
```

Архитектура должна быть модульной.

Каждый этап должен иметь возможность тестироваться независимо.

---

# 5. Preprocessing

## 5.1 Audio normalization

Не изменять исходный audio buffer.

Для анализа создать внутреннее представление.

Предусмотреть:

* stereo → mono analysis signal;
* DC offset removal;
* optional loudness normalization;
* безопасный handling очень тихих сигналов;
* sample-rate independence.

Не использовать destructive processing.

---

# 6. Feature extraction

Основой анализа должен быть STFT.

Предусмотреть возможность нескольких resolution:

```text
short window
medium window
long window
```

Конкретные параметры должны быть вынесены в configuration.

Feature extraction должен предоставлять:

```text
spectral magnitude
spectral flux
log spectral flux
mel-band energy
energy envelope
phase deviation
complex-domain novelty
```

Не делать всю систему зависимой от одного feature.

---

# 7. Multi-band analysis

Разделить сигнал минимум на:

```text
low
mid
high
full-band
```

Ориентировочно:

```text
low  = ~20–150 Hz
mid  = ~150–2000 Hz
high = ~2–12 kHz
```

Границы должны быть configurable.

Для каждого диапазона построить собственную onset/novelty representation.

Например:

```text
O_low(t)
O_mid(t)
O_high(t)
O_full(t)
```

---

# 8. Onset detection

Реализовать несколько источников onset evidence:

```text
spectral flux
log spectral flux
energy difference
phase deviation
complex-domain novelty
```

После этого объединять их:

```text
O(t) = weighted fusion(
    O_low,
    O_mid,
    O_high,
    O_full,
    ...
)
```

Не использовать жёсткие постоянные веса без возможности настройки.

По возможности система должна определять наиболее информативные frequency bands для конкретного материала.

---

# 9. Onset envelope

Onset envelope должен:

* быть устойчивым к изменению громкости;
* подавлять постоянный harmonic content;
* подчёркивать transient events;
* не создавать чрезмерное количество ложных onset'ов.

Предусмотреть:

```text
normalization
compression
adaptive thresholding
peak picking
```

Но raw onset representation должна сохраняться для последующих алгоритмов.

---

# 10. Rhythmicity estimation

Перед попыткой определения BPM оценить, насколько аудио вообще содержит выраженную периодическую ритмическую структуру.

Результат:

```text
rhythmicity ∈ [0, 1]
```

Примеры:

```text
drum loop       → high
full song       → high
ambient pad     → low
speech          → variable
single kick     → insufficient
noise           → low
```

Если rhythmicity слишком низкая, система должна иметь возможность вернуть:

```text
tempo = unknown
```

а не случайный BPM.

---

# 11. Tempo hypothesis generation

Использовать периодичность onset signal.

Основные методы:

```text
autocorrelation
tempogram
```

Рассматривать диапазон tempo:

```text
configurable minimum BPM
configurable maximum BPM
```

По умолчанию ориентироваться примерно на:

```text
40–240 BPM
```

Но не считать эти границы фундаментальным ограничением.

---

# 12. Log-tempo representation

Tempo hypotheses желательно представлять в log2 tempo space.

Причина:

```text
60 → 120 → 240
```

должны рассматриваться как одинаковые octave relationships.

Это упростит:

* half-time detection;
* double-time detection;
* candidate clustering;
* scoring.

---

# 13. Tempo candidates

Генерировать не один BPM, а набор кандидатов:

```text
TempoCandidate {
    bpm
    periodicityScore
    onsetScore
    ...
}
```

Например:

```text
128 BPM
64 BPM
256 BPM
127.4 BPM
129.1 BPM
```

После генерации кандидаты должны быть кластеризованы.

---

# 14. Half-time / double-time resolution

Особое внимание уделить:

```text
60 ↔ 120 ↔ 240
```

Не выбирать максимальный autocorrelation peak напрямую.

Для каждого кандидата учитывать:

* onset alignment;
* beat consistency;
* accent structure;
* low-frequency rhythmic evidence;
* subdivision consistency;
* meter plausibility;
* temporal stability.

Система должна сохранять альтернативные интерпретации, если различие недостаточно уверенное.

---

# 15. Beat tracking

Для каждого значимого tempo candidate построить beat sequence.

Концептуально:

```text
b0
b1
b2
b3
...
```

Использовать dynamic programming или эквивалентный temporal optimization algorithm.

Целевая функция должна учитывать минимум:

```text
onset evidence
expected beat interval
interval deviation
phase consistency
```

Общая форма:

```text
Score(beats) =
    onsetReward
    - tempoDeviationPenalty
    - irregularityPenalty
```

---

# 16. Не требовать точного onset в beat position

Beat position не обязан совпадать с максимальным пиком onset envelope.

Использовать локальное temporal neighborhood.

Например:

```text
             beat
               ↓
        ┌─────────────┐
--------███████████████--------
```

а не только:

```text
--------█----------------------
```

Это необходимо для живого исполнения и imperfect timing.

---

# 17. Phase optimization

Для каждого tempo candidate отдельно оптимизировать phase.

Например, при:

```text
BPM = 120
period = 0.5 sec
```

нужно определить:

```text
phase = 0.000 ... 0.500 sec
```

которая лучше всего объясняет ритмические события.

Не считать первый сильный transient автоматически первым beat.

---

# 18. Beat trajectory

Хранить реальные beat positions отдельно от глобального BPM.

Например:

```text
global BPM = 98.2

beats:
0.000
0.615
1.224
1.840
2.447
...
```

Это позволяет учитывать groove и tempo drift.

---

# 19. Tempo curve

После получения beat grid вычислять локальный tempo:

```text
tempo[i] = 60 / (beat[i+1] - beat[i])
```

Построить:

```text
tempoCurve(t)
```

Предусмотреть robust smoothing.

Не уничтожать реальные tempo changes чрезмерным smoothing.

---

# 20. Tempo stability

Рассчитать:

```text
tempoStability ∈ [0, 1]
```

Высокая стабильность:

```text
120
120
119.9
120.1
120
```

Низкая:

```text
120
124
118
127
121
```

Stability должна учитывать длину доступного материала.

---

# 21. Downbeat detection

Добавить отдельный слой для определения downbeats.

Не считать каждый beat равнозначным.

Использовать:

* accent structure;
* low-frequency energy;
* spectral novelty;
* harmonic changes;
* periodicity;
* beat-relative patterns.

Результат:

```text
downbeats[]
```

Если уверенность недостаточна:

```text
downbeats = []
```

или соответствующий optional state.

---

# 22. Meter estimation

Предусмотреть оценку:

```text
2/4
3/4
4/4
6/8
...
```

Но meter не должен быть обязательным условием для определения BPM.

Результат:

```text
meter {
    value
    confidence
}
```

Если meter ambiguous — сохранять ambiguity.

---

# 23. Segmentation

Для длинного audio не считать весь файл одной однородной структурой.

Разделить анализ на локальные окна:

```text
4 sec
8 sec
16 sec
32 sec
```

или другую адаптивную схему.

Получить:

```text
localTempo(t)
localRhythmicity(t)
localConfidence(t)
```

Это позволит корректно работать с:

* intro;
* outro;
* tempo changes;
* sections без drums;
* rubato;
* pauses.

---

# 24. Global tempo estimation

Global BPM получать не простым средним локальных BPM.

Использовать weighted aggregation с учётом:

```text
rhythmicity
confidence
duration
stability
```

Участки с отсутствующей ритмической информацией не должны сильно влиять на global tempo.

---

# 25. Confidence model

Система должна предоставлять confidence минимум для:

```text
tempo
beat grid
phase
downbeat
meter
```

Confidence должен учитывать:

```text
periodicity
onset strength
beat alignment
tempo stability
candidate separation
rhythmicity
amount of usable audio
```

Важно:

> Confidence должен отражать неопределённость алгоритма, а не просто силу найденного autocorrelation peak.

---

# 26. Alternative tempo candidates

Если присутствует ambiguity:

```text
tempo:
    128 BPM
    confidence: 0.71

alternatives:
    64 BPM
    256 BPM
```

Если две интерпретации почти эквивалентны, не скрывать вторую.

Это особенно важно для пользовательского sampling workflow.

---

# 27. One-shot и неритмический материал

Для:

```text
kick.wav
snare.wav
vocal phrase
ambient.wav
noise.wav
```

алгоритм не должен насильно назначать BPM.

Допустимый результат:

```text
tempo = unknown
```

с диагностикой причины:

```text
insufficient_periodicity
low_rhythmicity
insufficient_duration
ambiguous_tempo
```

---

# 28. Loop detection

Архитектура должна предусматривать возможность следующего слоя:

```text
detectLoops()
```

На основе:

* beat grid;
* bars;
* waveform similarity;
* spectral similarity;
* phase alignment.

Пример результата:

```text
LoopCandidate {
    start
    end
    beats
    bars
    confidence
}
```

Это не обязательно реализовывать в первой версии, но API анализа должен позволять это добавить без переписывания TempoEngine.

---

# 29. Warp map

Предусмотреть возможность построения:

```text
WarpMap
```

из:

```text
audio time
→ musical time
```

Например:

```text
audioTime  → beatPosition

0.000      → 0
0.615      → 1
1.224      → 2
1.840      → 3
...
```

Это должно стать основой будущего:

* time stretching;
* beat sync;
* elastic audio;
* sample alignment.

---

# 30. Architecture for future ML

Не делать первую реализацию полностью neural.

Архитектура должна позволять позднее добавить:

```text
NeuralBeatEstimator
```

который выдаёт:

```text
P(beat | frame)
P(downbeat | frame)
P(onset | frame)
```

Эти probability curves должны поступать в существующий temporal decoder.

Целевая архитектура:

```text
DSP features
      +
Neural predictions
      ↓
Temporal decoder
      ↓
Beat grid
```

Нейросеть не должна полностью заменять temporal reasoning.

---

# 31. Рекомендуемая реализация V1

Первая production-capable версия должна содержать:

```text
1. STFT

2. Multi-band decomposition

3. Spectral flux

4. Energy novelty

5. Phase/complex novelty

6. Onset fusion

7. Rhythmicity estimation

8. Autocorrelation

9. Tempogram

10. Tempo hypothesis generation

11. Candidate clustering

12. Dynamic-programming beat tracker

13. Phase optimization

14. Half/double-time resolution

15. Tempo stability

16. Confidence estimation
```

Не начинать с ML.

---

# 32. V2

После стабильной V1 добавить:

```text
Neural beat probability
Neural downbeat probability
Meter estimation
Tempo curve refinement
Loop detection
Warp map
```

---

# 33. Performance requirements

Система должна иметь два режима.

## FAST

Используется автоматически при drag-and-drop.

Цель:

```text
минимальная latency
```

Разрешается использовать:

* reduced feature resolution;
* simplified candidate search;
* shorter analysis windows.

---

## DEEP

Используется для окончательного анализа.

Включает:

* multi-resolution analysis;
* более точный beat tracking;
* расширенный candidate search;
* tempo curve;
* downbeat analysis;
* дополнительные confidence calculations.

API должен позволять:

```text
analyze(audio, mode=FAST)
analyze(audio, mode=DEEP)
```

---

# 34. Diagnostics

TempoEngine должен предоставлять debug information.

Минимум:

```text
onsetEnvelope
tempogram
tempoCandidates
beatPositions
tempoCurve
rhythmicity
confidence
```

В debug build должна существовать возможность визуализировать:

```text
waveform
onset envelope
candidate tempos
selected tempo
beats
downbeats
tempo curve
```

Это критически важно для разработки.

Не делать алгоритм black box.

---

# 35. Тестовый набор

Создать regression test corpus.

Минимальные категории:

### Electronic

```text
straight 4/4
half-time
double-time
syncopated
```

### Hip-hop

```text
breakbeats
sampled drums
swing
```

### Funk

```text
humanized timing
ghost notes
syncopation
```

### Jazz

```text
swing
live tempo
rubato
```

### Rock

```text
steady 4/4
fills
```

### Classical / live

```text
tempo changes
rubato
```

### Vocals

```text
vocal-only
spoken word
```

### Non-rhythmic

```text
ambient
noise
field recordings
single hits
sustained pads
```

---

# 36. Обязательные adversarial tests

Особенно тщательно тестировать:

```text
60 vs 120 BPM

70 vs 140 BPM

80 vs 160 BPM

swing

triplets

3:2 polyrhythm

missing kick

missing snare

strong offbeat

long intro

long silence

tempo drift

abrupt tempo change

very short sample

single transient

dense percussion

harmonic music without drums
```

---

# 37. Метрики качества

Не оценивать систему только по BPM error.

Использовать минимум:

```text
Tempo Accuracy
Beat F-measure
Beat phase accuracy
Downbeat accuracy
Tempo stability error
False tempo rate
Unknown detection accuracy
```

Особенно важна метрика:

```text
False Positive Tempo Rate
```

То есть как часто система уверенно придумывает BPM там, где музыкального темпа фактически нет.

---

# 38. Acceptance criteria

Система считается готовой к интеграции в DAW, когда:

1. Она стабильно определяет BPM на регулярном ритмическом материале.
2. Не путает большинство half/double-time случаев.
3. Возвращает beat grid, а не только BPM.
4. Корректно определяет phase.
5. Умеет сообщать uncertainty.
6. Не назначает случайный BPM неритмическому материалу.
7. Может показать несколько tempo candidates.
8. Может обнаруживать tempo drift.
9. Не зависит от конкретного sample rate.
10. Имеет regression test suite.
11. Имеет debug visualization.
12. Каждый основной этап pipeline можно протестировать отдельно.

---

# 39. Главный принцип разработки

Не оптимизировать алгоритм под:

```text
"получить правильное число BPM"
```

Оптимизировать под:

```text
"восстановить наиболее вероятную музыкальную временную структуру аудио"
```

BPM — только одно из представлений этой структуры.

Иерархия должна быть:

```text
Audio
 ↓
Events
 ↓
Periodicity
 ↓
Tempo hypotheses
 ↓
Beat sequence
 ↓
Phase
 ↓
Meter / downbeats
 ↓
Tempo trajectory
 ↓
Musical time representation
```

---

# 40. Задача AI-агента

Ты являешься AI software engineer, реализующим данный модуль в существующем DAW.

Перед написанием кода:

1. Изучи существующую архитектуру проекта.
2. Найди audio engine, DSP utilities и существующие abstractions.
3. Определи подходящее место для `MusicalTimeAnalysis`.
4. Не дублируй существующие DSP utilities.
5. Определи API, совместимый с текущей архитектурой.
6. Составь короткий implementation plan.
7. Только после этого начинай реализацию.

Во время реализации:

* сохраняй модульность;
* не создавай giant class;
* не смешивай audio decoding, DSP, tempo inference и UI;
* добавляй unit tests вместе с функциональностью;
* избегай premature optimization;
* сохраняй промежуточные результаты анализа;
* делай алгоритм наблюдаемым и debug-friendly.

При неоднозначных результатах не скрывай uncertainty.

Не подменяй отсутствие информации случайным BPM.

---

# 41. Порядок реализации

Работать итеративно:

### Phase 1

```text
Audio preprocessing
STFT
spectral features
```

### Phase 2

```text
multi-band onset detection
onset envelope
```

### Phase 3

```text
autocorrelation
tempogram
tempo candidates
```

### Phase 4

```text
beat tracker
phase optimization
```

### Phase 5

```text
half/double-time resolution
confidence
```

### Phase 6

```text
local tempo
tempo stability
```

### Phase 7

```text
downbeats
meter
```

### Phase 8

```text
loop detection
warp map
```

### Phase 9

```text
optional neural estimator
```

После каждой фазы запускать regression tests.

---

# 42. Важное ограничение

Не добавлять зависимости только ради реализации одного небольшого алгоритма.

Перед добавлением сторонней библиотеки:

1. проверить существующие зависимости проекта;
2. проверить возможность реализации средствами текущего DSP stack;
3. оценить licensing;
4. оценить realtime/performance implications;
5. оценить возможность дальнейшей поддержки.

---

# 43. Итоговая архитектурная цель

В конечном итоге DAW должен воспринимать импортированный sample не как:

```text
AudioFile + BPM
```

а как:

```text
AudioFile
    +
MusicalTimeModel
```

где:

```text
MusicalTimeModel {

    globalTempo

    tempoCandidates

    beatGrid

    downbeatGrid

    meter

    tempoCurve

    rhythmicity

    confidence

    warpMap
}
```

Этот объект должен стать фундаментом sampling workflow DAW.

Он должен быть пригоден для последующего построения:

```text
automatic looping
beat slicing
sample quantization
tempo sync
time stretching
warp editing
beat matching
sample browser metadata
```

Не ограничивать архитектуру текущей задачей BPM detection.
