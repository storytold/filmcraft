//! The HEVC inverse transform matrices (8.6.4.2), shared by the vector kernels.

/// The magnitudes of the DCT basis, 64 * sqrt(2) * cos(k * pi / 64) rounded as the specification's
/// transMatrix has them, for k = 0..32 (k = 0: the DC row's 64).
const COS: [i16; 32] = [64, 90, 90, 90, 89, 88, 87, 85, 83, 82, 80, 78, 75, 73, 70, 67, 64, 61, 57, 54, 50, 46, 43, 38, 36, 31, 25, 22, 18, 13, 9, 4];

/// The signed basis value at angle k * pi / 64 (k modulo 128: a whole turn).
const fn cos(k: usize) -> i16 {
    let k = k % 128;
    match k {
        0..32 => COS[k],
        32 | 96 => 0,
        33..64 => -COS[64 - k],
        64..96 => -COS[k - 64],
        _ => COS[128 - k],
    }
}

/// transMatrix of 8.6.4.2 with row j = basis function j: DCT[j][y] = cos(j * (2y + 1)). The
/// N-point transforms use rows j * 32 / N and their first N samples.
pub(crate) static DCT: [[i16; 32]; 32] = {
    let mut m = [[0i16; 32]; 32];
    let mut j = 0;
    while j < 32 {
        let mut y = 0;
        while y < 32 {
            m[j][y] = cos(j * (2 * y + 1));
            y += 1;
        }
        j += 1;
    }
    m
};

/// The DST-VII matrix of 8.6.4.2 (trType 1), row j = basis function j.
pub(crate) static DST: [[i16; 4]; 4] = [[29, 55, 74, 84], [74, 74, 0, -74], [84, -29, -74, 55], [55, -84, 74, -29]];
