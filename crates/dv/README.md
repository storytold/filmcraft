# Native DV25 / DVCPRO25 decoding

Clean-room pure-Rust decoder for the 525/60, 25 Mb/s, 4:1:1 video used by
NTSC DV/DVCPRO QuickTime (`dvc `) footage. Implemented from **SMPTE 314M-2005**
(stabilized 2013), sections 4.7 and 5, tables 23–25 and figures 20, 22, 26–28, 30, 32.
Specification: https://pub.smpte.org/pub/st314/st0314-2005_stable2013.pdf

Implements DIF indexing, three-pass AC bit allocation, VLC and escape codes,
inverse quantization/weighting, 8x8 and 2-4-8 inverse DCT, macroblock unshuffling,
and the clipped rightmost chroma blocks. Four abbreviated printed VLC words
are expanded to Table 24's lengths to obtain prefix-free codes, validated against
the external oracle. All frame sizes, coefficient runs and block locations are bounded.

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
difference 3, mean square error below 0.5), and bit-flip/truncation mutation tests.
The standalone example decodes DIF test fixtures to planar YUV for oracle comparisons.
