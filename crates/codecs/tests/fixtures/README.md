# MJPEG decoder fixtures

`mjpeg-avi1-baseline.jpg` is `testorig.jpg` from libjpeg-turbo 3.0.0 `testimages/`, under the
IJG License included in `README.ijg`. The source is documented in the [libjpeg-turbo 3.0.0
fixture directory](https://github.com/libjpeg-turbo/libjpeg-turbo/tree/3.0.0/testimages).

`mjpeg-progressive-huffman.jpg` is derived from the same image: FFmpeg decoded it to PPM, then
libjpeg-turbo `cjpeg -progressive -quality 88` encoded it as SOF2. It is covered by the same
license. `mjpeg-cmyk.jpg` is the 16×16 CMYK fixture from craft-libs `crates/prepress/tests/fixtures`.
