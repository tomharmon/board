# Generated media fixtures

`magenta.avif` is a single 24 x 16 solid-color test frame. `animated.webp`
is cyan for the first quarter second and then magenta. The test generates
equivalent GIF frames in memory.

```sh
ffmpeg -v error -f lavfi -i color=c=magenta:s=24x16 -frames:v 1 -threads 1 \
  -c:v libaom-av1 -still-picture 1 magenta.avif
ffmpeg -v error -f lavfi -i color=c=cyan:s=24x16:r=4:d=0.25 \
  -f lavfi -i color=c=magenta:s=24x16:r=4:d=2 \
  -filter_complex '[0:v][1:v]concat=n=2:v=1:a=0[v]' -map '[v]' \
  -threads 1 -c:v libwebp_anim -lossless 1 -loop 1 animated.webp
```

`swatches.mp4` comprises two seconds of generated flat color at 2 fps (cyan then magenta), 64 x 48, H.264 with no audio. There is no third-party footage.

```sh
ffmpeg -v error -f lavfi -i color=c=cyan:size=64x48:rate=2:duration=1 \
  -f lavfi -i color=c=magenta:size=64x48:rate=2:duration=1 \
  -filter_complex '[0:v][1:v]concat=n=2:v=1:a=0[v]' -map '[v]' \
  -c:v libx264 -movflags +faststart -threads 1 swatches.mp4
```

`timeline.webm` has ten seconds of silent VP9, changing from cyan to magenta five seconds in. `green-vp8.webm` is a two-second VP8 clip used in the live permission test. The two sound fixtures pair generated solid colors with generated sine tones, not third-party media.

```sh
ffmpeg -v error -f lavfi -i color=c=cyan:size=64x48:rate=12:duration=5 \
  -f lavfi -i color=c=magenta:size=64x48:rate=12:duration=5 \
  -filter_complex '[0:v][1:v]concat=n=2:v=1:a=0[v]' -map '[v]' \
  -c:v libvpx-vp9 -g 12 -threads 1 -pix_fmt yuv420p -an -metadata title=board-test-swatches timeline.webm
ffmpeg -v error -f lavfi -i color=c=green:size=64x48:rate=12:duration=2 \
  -c:v libvpx -g 12 -threads 1 -pix_fmt yuv420p -an green-vp8.webm
ffmpeg -v error -f lavfi -i color=c=royalblue:size=64x48:rate=12:duration=12 \
  -f lavfi -i sine=frequency=440:sample_rate=24000:duration=12 \
  -c:v libvpx-vp9 -g 12 -threads 1 -c:a libopus -b:a 16k -shortest tone.webm
ffmpeg -v error -f lavfi -i color=c=lime:size=64x48:rate=12:duration=4 \
  -f lavfi -i sine=frequency=660:sample_rate=24000:duration=4 \
  -c:v libx264 -g 12 -threads 1 -c:a aac -b:a 24k -movflags +faststart -shortest tone-aac.mp4
```
