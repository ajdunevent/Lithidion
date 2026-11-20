# voiceprobe

A development tool, not part of Lithidion. It loads one speech-to-text model and transcribes WAV
clips with it, to show whether a machine can run the model Lithidion would use for watch
dictation, and how well the model hears the kind of thing said into a watch. It uses the same
engine and version as Lithidion would (`transcribe-cpp`, from the workspace's `Cargo.lock`).

For each run it reports the machine, the model's load time and memory, and for each clip its
length, how long it took to transcribe as a fraction of that length (below 1.00× is faster than
the clip itself), and the text. It ends with the overall rate and the most memory the process
used.

## Running it

From the project root:

```sh
cargo run --release -p voiceprobe -- [options] MODEL.gguf CLIP.wav...
cargo run --release -p voiceprobe -- --help
```

In a development Flatpak (made by `build-aux/dev.sh`; the published build leaves the probe out),
run it inside Lithidion's sandbox, giving it the folder that holds the model and clips:

```sh
flatpak run --filesystem=~/voiceprobe --command=voiceprobe net.dunevent.Lithidion \
    ~/voiceprobe/model.gguf ~/voiceprobe/*.wav
```

| Option | What it does |
|---|---|
| `--threads N` | CPU threads (default: the library's choice) |
| `--runs N` | Transcribe each clip N times and also report the fastest; the first run includes warm-up |
| `--language CODE` | Language hint, such as `en`, for models that take one |
| `--verbose` | Show the speech library's own diagnostics |

The native engine is always compiled optimised, so timings are meaningful from any build profile.

## Models

GGUF files from [handy-computer on Hugging Face](https://huggingface.co/handy-computer); each
model's page in the `transcribe.cpp` docs lists the files. Q8_0 is the usual choice. Candidates
for a phone, smallest first:

| Model | File | Size | Published WER | Notes |
|---|---|---|---|---|
| Moonshine tiny | `moonshine-tiny-gguf/moonshine-tiny-Q8_0.gguf` | 35 MB | 4.60% | English; short utterances only |
| Moonshine base | `moonshine-base-gguf/moonshine-base-Q8_0.gguf` | 77 MB | 3.26% | English; short utterances only |
| Whisper base.en | `whisper-base.en-gguf/whisper-base.en-Q8_0.gguf` | 85 MB | 4.16% | English; a familiar reference |
| Parakeet TDT-CTC 110M | `parakeet-tdt_ctc-110m-gguf/parakeet-tdt_ctc-110m-Q8_0.gguf` | 135 MB | 2.43% | English |
| Parakeet TDT 0.6B v3 | `parakeet-tdt-0.6b-v3-gguf/parakeet-tdt-0.6b-v3-Q8_0.gguf` | 740 MB | 1.94% | 25 languages; the large end |

The error rates are those in each model's page of the `transcribe.cpp` docs, which name the test
set.

Each is downloaded from `https://huggingface.co/handy-computer/<file>` with `resolve/main/` before
the file name, for example:

```sh
curl -L -o ~/voiceprobe/parakeet-110m.gguf \
    https://huggingface.co/handy-computer/parakeet-tdt_ctc-110m-gguf/resolve/main/parakeet-tdt_ctc-110m-Q8_0.gguf
```

## Clips

The watch records at 16 kHz in one channel, so record clips the same way:

```sh
pw-record --rate 16000 --channels 1 ~/voiceprobe/lights.wav
```

Other WAV files work too; they are mixed to one channel and resampled (simply, so a 16 kHz
recording is the fairer test), and the report says so. Short, watch-like phrases tell the most:
a Home Assistant request, a text reply, a name. Moonshine models stop on long clips (about
half a minute or more); the report says the clip was not transcribed and carries on.
