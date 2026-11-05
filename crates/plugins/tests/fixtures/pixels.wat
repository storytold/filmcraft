;; A small real data plane plugin. `craft_process` picks its work from the descriptor's `entry`:
;;   0 copy                      out = in
;;   1 vsum                      out[y] = in[y - 1] + in[y + 1] per byte, wrapping, y clamped to the band
;;   2 nan                       copy, then out sample 0 = NaN and sample 1 = +infinity (f32)
;;   3 x4                        every f32 sample times 4
;;   4 trap                      unreachable
;;   5 err                       returns 7
;;   6 spin                      never returns
;;   7 params                    copies the params JSON to the first inner pixel of the output
;;   8 desc                      copies the 96 byte descriptor to the first inner pixel of the output
;;   9 big                       writes 255 into every byte (a u16 image reads that as 65535)
;;  10 trap-low                  copies, but traps when the band starts below document row 10
;; "First inner pixel" is where the host keeps the band: `overlap` rows and `overlap` pixels in.
;; Any other entry returns 9. The allocator never frees; each band runs on a fresh instance.
;; Descriptor offsets: entry 8, w 12, h 16, stride 20, in_ptr 80, out_ptr 84, params_ptr 88, params_len 92.
(module
  (memory (export "memory") 16 64)
  (global $heap (mut i32) (i32.const 4096))

  (func $alloc (param $len i32) (param $align i32) (result i32)
    (local $p i32)
    (local.set $p (global.get $heap))
    (global.set $heap (i32.add (global.get $heap) (local.get $len)))
    (if (i32.gt_u (global.get $heap) (i32.const 1000000)) (then (global.set $heap (local.get $p)) (return (i32.const 0))))
    (local.get $p))

  (func (export "craft_abi") (result i32) i32.const 1)
  (export "craft_alloc" (func $alloc))
  (func (export "craft_free") (param i32 i32 i32))

  (func (export "craft_process") (param $dp i32) (param $dl i32) (result i32)
    (local $entry i32) (local $h i32) (local $stride i32) (local $in i32) (local $out i32)
    (local $pp i32) (local $pl i32) (local $n i32) (local $y i32) (local $i i32) (local $up i32) (local $dn i32)
    (local $w i32) (local $inner i32)
    (local.set $entry (i32.load (i32.add (local.get $dp) (i32.const 8))))
    (local.set $h (i32.load (i32.add (local.get $dp) (i32.const 16))))
    (local.set $stride (i32.load (i32.add (local.get $dp) (i32.const 20))))
    (local.set $in (i32.load (i32.add (local.get $dp) (i32.const 80))))
    (local.set $out (i32.load (i32.add (local.get $dp) (i32.const 84))))
    (local.set $pp (i32.load (i32.add (local.get $dp) (i32.const 88))))
    (local.set $pl (i32.load (i32.add (local.get $dp) (i32.const 92))))
    (local.set $n (i32.mul (local.get $stride) (local.get $h)))
    (local.set $w (i32.load (i32.add (local.get $dp) (i32.const 12))))
    ;; out + overlap * stride + overlap * bytes_per_pixel
    (local.set $inner (i32.add (local.get $out)
      (i32.add
        (i32.mul (i32.load (i32.add (local.get $dp) (i32.const 52))) (local.get $stride))
        (i32.mul (i32.load (i32.add (local.get $dp) (i32.const 52))) (i32.div_u (local.get $stride) (local.get $w))))))

    (if (i32.eq (local.get $entry) (i32.const 0)) (then
      (memory.copy (local.get $out) (local.get $in) (local.get $n))
      (return (i32.const 0))))

    (if (i32.eq (local.get $entry) (i32.const 1)) (then
      (local.set $y (i32.const 0))
      (block $rows (loop $row
        (br_if $rows (i32.ge_u (local.get $y) (local.get $h)))
        (local.set $up (select (i32.sub (local.get $y) (i32.const 1)) (i32.const 0) (i32.gt_u (local.get $y) (i32.const 0))))
        (local.set $dn (select (i32.add (local.get $y) (i32.const 1)) (i32.sub (local.get $h) (i32.const 1))
                               (i32.lt_u (i32.add (local.get $y) (i32.const 1)) (local.get $h))))
        (local.set $i (i32.const 0))
        (block $cols (loop $col
          (br_if $cols (i32.ge_u (local.get $i) (local.get $stride)))
          (i32.store8
            (i32.add (local.get $out) (i32.add (i32.mul (local.get $y) (local.get $stride)) (local.get $i)))
            (i32.add
              (i32.load8_u (i32.add (local.get $in) (i32.add (i32.mul (local.get $up) (local.get $stride)) (local.get $i))))
              (i32.load8_u (i32.add (local.get $in) (i32.add (i32.mul (local.get $dn) (local.get $stride)) (local.get $i))))))
          (local.set $i (i32.add (local.get $i) (i32.const 1)))
          (br $col)))
        (local.set $y (i32.add (local.get $y) (i32.const 1)))
        (br $row)))
      (return (i32.const 0))))

    (if (i32.eq (local.get $entry) (i32.const 2)) (then
      (memory.copy (local.get $out) (local.get $in) (local.get $n))
      (f32.store (local.get $out) (f32.const nan))
      (f32.store (i32.add (local.get $out) (i32.const 4)) (f32.const inf))
      (return (i32.const 0))))

    (if (i32.eq (local.get $entry) (i32.const 3)) (then
      (local.set $i (i32.const 0))
      (block $done (loop $l
        (br_if $done (i32.ge_u (local.get $i) (local.get $n)))
        (f32.store (i32.add (local.get $out) (local.get $i))
          (f32.mul (f32.load (i32.add (local.get $in) (local.get $i))) (f32.const 4)))
        (local.set $i (i32.add (local.get $i) (i32.const 4)))
        (br $l)))
      (return (i32.const 0))))

    (if (i32.eq (local.get $entry) (i32.const 4)) (then unreachable))
    (if (i32.eq (local.get $entry) (i32.const 5)) (then (return (i32.const 7))))
    (if (i32.eq (local.get $entry) (i32.const 6)) (then (loop $spin (br $spin))))

    (if (i32.eq (local.get $entry) (i32.const 7)) (then
      (memory.copy (local.get $inner) (local.get $pp) (local.get $pl))
      (return (i32.const 0))))

    (if (i32.eq (local.get $entry) (i32.const 8)) (then
      (memory.copy (local.get $inner) (local.get $dp) (i32.const 96))
      (return (i32.const 0))))

    (if (i32.eq (local.get $entry) (i32.const 9)) (then
      (memory.fill (local.get $out) (i32.const 255) (local.get $n))
      (return (i32.const 0))))

    (if (i32.eq (local.get $entry) (i32.const 10)) (then
      (if (i32.gt_s (i32.load (i32.add (local.get $dp) (i32.const 48))) (i32.const 10)) (then unreachable))
      (memory.copy (local.get $out) (local.get $in) (local.get $n))
      (return (i32.const 0))))

    (i32.const 9))
)
