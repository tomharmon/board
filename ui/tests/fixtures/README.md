# Video sample fixture

`swatches.mp4` comprises two seconds of generated flat color at 2 fps (cyan then magenta), 64 x 48, H.264 with no audio. There is no third-party footage.

```sh
ffmpeg -v error -f lavfi -i color=c=cyan:size=64x48:rate=2:duration=1 \
  -f lavfi -i color=c=magenta:size=64x48:rate=2:duration=1 \
  -filter_complex '[0:v][1:v]concat=n=2:v=1:a=0[v]' -map '[v]' \
  -c:v libx264 -movflags +faststart -threads 1 swatches.mp4
```
