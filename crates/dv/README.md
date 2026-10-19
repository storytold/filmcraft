# Native DV25 / DVCPRO25 decoding

Clean-room pure-Rust decoder for the 525/60, 25 Mb/s, 4:1:1 video used by
NTSC DV/DVCPRO QuickTime (`dvc `) footage. Implemented from **SMPTE 314M-2005**
(stabilized 2013), sections 4.7 and 5, tables 23–25 and figures 20, 22, 26–28, 30, 32.
Specification: https://pub.smpte.org/pub/st314/st0314-2005_stable2013.pdf

Implements DIF indexing, three-pass AC bit allocation, VLC and escape codes,
inverse quantization/weighting, 8x8 and 2-4-8 inverse DCT, macroblock unshuffling,
and the clipped rightmost chroma blocks. Four abbreviated printed VLC words
(run 7 and 8 with amplitude 1; run 1 with amplitudes 16 and 17) are expanded to
Table 24's lengths to obtain prefix-free codes. FFmpeg's encoder emits all four, and
they are frequent in camera footage, which decodes within the same tolerance of FFmpeg
(not committed). All frame sizes, coefficient runs and block locations are bounded.

Returns original 720x480, 8-bit planar 4:1:1 pictures with the original field order.
FilmCraft's adapter represents the chroma in its existing 4:2:2 compositor by
replicating samples; the MOV source continues handling clean aperture, pixel aspect,
timecode, audio and seeking. No external decoder, transcoded file, or FFmpeg runtime
dependency is used. PCM audio uses FilmCraft's existing native decoder.

Currently scoped to NTSC DV25/DVCPRO25 MOV. PAL DV, DV50/DVCPRO50, DVCPRO HD,
raw DV audio demultiplexing and AVI DV are not implemented. Unsupported or damaged
packets return a decoding error without changing the input or crashing.

`cargo test -p filmcraft-dv` covers spatial coverage, malformed inputs, generated
progressive/interlaced patterns versus an external FFmpeg oracle (maximum sample
difference 3, mean square error below 0.5, over the whole picture and over the
clipped rightmost chroma columns), and bit-flip/truncation mutation tests.
FFmpeg's encoder never codes chroma with the 2-4-8 DCT, so two patterns set the
mode bit of every chroma block before both decoders read them; the interlaced
pattern is encoded with `+ildct` so luma 2-4-8 blocks are exercised too. The
fixtures are generated into `target/fixtures/dv/` (`cargo xtask fixtures dv`), and
`FILMCRAFT_REQUIRE_ORACLES=1` turns a missing ffmpeg into a failure.
The standalone example decodes DIF test fixtures to planar YUV for oracle comparisons.
