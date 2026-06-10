// aquarelle — 共有「にじみ」WGSL フラグメント（48 タップ黄金角螺旋）。
//
// orber #239 / #250 で kako-jun が承認した「新にじみ」= per-primitive の本物の
// 空間ブラー（multi-tap 平均）+ bloom/halo の色 character を、共有にじみエンジン
// （aquarelle crate）へ取り出したもの。orber と additive（どちらも wgpu/GPU）が
// `include` し、blueprinter（CPU）は Rust port を使う。中身は orber の
// `crates/core/src/orb.wgsl` から **byte 等価** に移植した（定数・式は一切いじらない）。
//
// **このフラグメントは「にじみ」だけを担う**。「ぼやけ」（= orb 縁の柔らかさ /
// falloff_curve）は各アプリに残す。にじみは形を formless 化する空間ブラーであり、
// 読みやすさは各アプリ側の「前面に元を重ねる」合成で担保する。
//
// box blur（旧 `render_aquarelle_bleed_pass`）とは別系統。box は温存する。
//
// === ホスト供給を前提とするシンボル（このフラグメント内で再定義しない）===
//
// 消費側シェーダ（orber/additive）が `AQUA_BLEED_WGSL` を結合する前に、以下を
// **必ず先に定義** しておくこと。署名は厳守:
//
//   const TAU: f32;               // = 6.28318530718（2π）
//
//   fn hash21(p: vec2<f32>) -> f32;
//       // per-pixel ハッシュ（0..1）。orb.wgsl の実装:
//       //   let h = dot(p, vec2<f32>(127.1, 311.7));
//       //   return fract(sin(h) * 43758.5453123);
//       // にじみのスパイラル初期角を画素ごとにずらしてトゲ状アーティファクトを
//       // 散らす。CPU port (aqua_hash21) と bit-for-bit 一致させること。
//
//   fn clampf(x: f32, a: f32, b: f32) -> f32;  // = min(max(x, a), b)
//
//   fn coverage_at(
//       style_bit: f32, sample_px: vec2<f32>,
//       cx: f32, cy: f32, radius: f32, blur: f32, opacity: f32, angle: f32,
//   ) -> vec2<f32>;
//       // variant 固有（orb=円距離 / glyph・image=SDF サンプル）。サンプル位置
//       // `sample_px` でのシルエット距離 r を falloff_curve に通し
//       // (straight alpha, rgb_scale) を返す。これが「orb に別のシルエットを
//       // 食わせる」唯一の差分点。blurred_coverage は複数タップでこれを平均する。
//
// `clampf` はこのフラグメントの関数本体では使わないが、orber では aqua_seed_dir /
// blurred_coverage と同じ翻訳単位に同居するため、共有シンボルとして明記しておく。

// ブラーのタップ数。静止画 PoC なので品質優先で多めに取る（重くてよい）。
const AQUA_BLUR_TAPS: u32 = 48u;
// 黄金角（ラジアン）。disk 内に等密度でタップを撒くスパイラルに使う（規則格子を避ける）。
const AQUA_GOLDEN_ANGLE: f32 = 2.39996323;
// bloom の白へ寄せる上限（offset=1 でもこの比までしか白くしない）。控えめにして
// 強い白飛びを禁止する（kako-jun「控えめに」）。
const BLOOM_MAX: f32 = 0.45;
// halo の彩度ゲイン係数（halo=1・縁で luma 軸からの距離をこの倍まで伸ばす上限）。
const HALO_SAT_GAIN: f32 = 0.6;
// offset の disk 原点バイアス量（blur_px に対する比。offset=1 でこの比だけ seed 方向へ
// ずらす）。形は壊さず滲みを非対称にする程度に控えめ。
const AQUA_OFFSET_BIAS: f32 = 0.6;

// #239 ブラー経路の空間早期カル用の「シルエット最大到達」（radius 倍）。被覆が確実に 0 に
// なる距離の上限を **安全側に大きく** 取るための定数。出力は一切変えない（カルするのは
// もともと alpha=0 になる画素だけ）。両 variant の最大到達を包む保守値:
//   - orb variant   : r=distance/radius、coverage_at は r>=1 で 0 → 到達 = radius（係数 1.0）。
//   - SDF variant   : サンプル箱は半幅 radius/CONTENT_SPAN（CONTENT_SPAN=1/√2）の正方形。
//                     中心からの最遠点は角で radius/CONTENT_SPAN * √2 = radius*2 → 係数 2.0。
// 共有 composite_straight は両 variant に展開されるので、大きい方（2.0）を採る。
const AQUA_REACH_RADIUS_FACTOR: f32 = 2.0;
// タップが sample_px から離れうる最大比（blur_px 倍）。disk 半径 blur_px + offset bias
// （最大 AQUA_OFFSET_BIAS*blur_px=0.6*blur_px）→ 1.0 + 0.6 = 1.6。
const AQUA_REACH_BLUR_FACTOR: f32 = 1.6;
// カル境界に足す安全マージン（px）。丸め・補間の縁を確実に内側へ寄せ、寄与しうる画素を
// 絶対にカルしないための保険。出力は不変なのでいくら大きくても正しさは保たれる。
const AQUA_CULL_SAFETY_PX: f32 = 4.0;

// per-orb seed（phase 由来）から決定論的な単位方向ベクトルを作る。offset 軸が
// ブラーの disk 原点をこの向きへずらして滲みを左右非対称・有機的にするのに使う。
// hash で角度を散らすだけなので「形」は一切作らない（円へモーフしない）。
fn aqua_seed_dir(seed: f32) -> vec2<f32> {
    let a = hash21(vec2<f32>(seed * 12.9898, seed * 78.233 + 4.1)) * TAU;
    return vec2<f32>(cos(a), sin(a));
}

// 被覆 alpha を blur 半径 `blur_px` の disk 内で multi-tap 空間平均する（= ガウス近似ブラー）。
// `coverage_at` は variant ごとに差し替わる（orb=円距離 / SDF=サンプル距離）。返り値は
// plain と同じ (straight alpha, rgb_scale) の vec2。形は変えず被覆を空間平均するだけなので
// 星は星のままぼけ、強ブラーで自然に formless 化する（丸へモーフしない）。
// `seed` は per-orb（phase 由来）。スパイラルの初期角をずらして規則パターンを避ける。
//
// #239 offset 軸: `bias_px` は disk 原点（タップを撒く中心）に加える per-orb seed 方向の
// ずれ。**サンプル位置（cx,cy への距離評価）はそのまま**で、ブラーの“筆の置きどころ”だけ
// を seed 方向へ寄せるので、滲みが左右非対称になる。形（coverage_at の距離場）は不変＝
// 星は星のまま・円へモーフしない。bias_px=0（offset=0）で従来の対称ブラーと厳密に一致。
fn blurred_coverage(
    style_bit: f32,
    sample_px: vec2<f32>,
    cx: f32,
    cy: f32,
    radius: f32,
    blur: f32,
    opacity: f32,
    angle: f32,
    blur_px: f32,
    seed: f32,
    bias_px: vec2<f32>,
) -> vec2<f32> {
    // タップ 0 は中心。残りは黄金角スパイラルで disk(半径 blur_px) に等密度散布。
    var sum_a = 0.0;
    var sum_scaled = 0.0; // alpha * rgb_scale の総和（重み付き平均の分子）
    let n = AQUA_BLUR_TAPS;
    let nf = f32(n);
    // 初期角は per-orb seed に加え **per-pixel ハッシュ**でずらす。これで隣接画素が
    // 別のタップ位置を踏み、コヒーレントなスパイラルのトゲがディザされて滑らかになる。
    let ang0 = seed * AQUA_GOLDEN_ANGLE * 7.0 + hash21(sample_px) * TAU;
    // disk の中心を offset 軸の bias 分だけ seed 方向へずらす（offset=0 で bias_px=0）。
    let center = sample_px + bias_px;
    for (var k: u32 = 0u; k < n; k = k + 1u) {
        // r ∝ sqrt(k/n) で disk 内一様面積分布、角は黄金角で回す。
        let kf = f32(k);
        let rr = blur_px * sqrt((kf + 0.5) / nf);
        let th = ang0 + kf * AQUA_GOLDEN_ANGLE;
        let off = vec2<f32>(cos(th) * rr, sin(th) * rr);
        let sp = center + off;
        let cov = coverage_at(style_bit, sp, cx, cy, radius, blur, opacity, angle);
        sum_a = sum_a + cov.x;
        sum_scaled = sum_scaled + cov.x * cov.y;
    }
    let avg_a = sum_a / nf;
    var avg_scale = 1.0;
    if (sum_a > 0.0) {
        avg_scale = sum_scaled / sum_a; // alpha 重み付き平均の rgb_scale
    }
    return vec2<f32>(avg_a, avg_scale);
}

// #239 bloom/halo 軸（ブラー後の色味補正。各 coef=0 で恒等）。被覆 alpha `cov_a`
// （ブラー後）を中心度の代理にして、`color` を控えめに加工する:
//   - bloom: 内部（cov_a 高）で色を白へ寄せて柔らかい明るいコアにする。
//            t = bloom * smoothstep(0.18, 0.5, cov_a) を白との mix 比に使う（閾値は
//            ブラー後 alpha の実効レンジ基準。最大でも BLOOM_MAX=0.45 までしか白へ
//            寄せない＝強い白飛びを禁止）。
//   - halo : 外周の柔らかい縁（cov_a 低～中）で**彩度だけ**を上げる。枠（alpha）は作らない。
//            彩度ブースト量 = halo * edgeness。edgeness = smoothstep(0.45,0.05,cov_a) で
//            内部ほど 0、縁ほど 1。彩度は luma 軸からの距離を係数 (1+halo*k) 倍にする。
// 返り値は加工後の straight rgb。cov_a=0 の画素はそもそも合成側 `alpha>0` で弾かれる。
fn aqua_character(color: vec3<f32>, cov_a: f32, bloom: f32, halo: f32) -> vec3<f32> {
    var rgb = color;
    // ブラー後の被覆 alpha は orb の opacity（≈0.5 程度）で頭打ちになるため、中心度/縁度は
    // その実効レンジに合わせて閾値を取る（絶対 1.0 基準だと内部でもほぼ発火しない）。
    // --- halo: 外周の彩度ブースト（色味だけ。alpha リングは作らない）---
    if (halo > 0.0) {
        let edgeness = smoothstep(0.45, 0.05, cov_a); // 内部=0 → 縁=1
        let luma = dot(rgb, vec3<f32>(0.299, 0.587, 0.114));
        let sat_gain = 1.0 + halo * HALO_SAT_GAIN * edgeness; // 1.0 で恒等
        rgb = clamp(vec3<f32>(luma) + (rgb - vec3<f32>(luma)) * sat_gain, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    // --- bloom: 中心の柔らかい明るいコア（控えめに白へ）---
    if (bloom > 0.0) {
        let centerness = smoothstep(0.18, 0.5, cov_a); // 縁=0 → 中心=1（実効レンジ基準）
        let t = bloom * BLOOM_MAX * centerness; // 最大 BLOOM_MAX までしか白へ寄せない
        rgb = mix(rgb, vec3<f32>(1.0), t);
    }
    return rgb;
}
