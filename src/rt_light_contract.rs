//! CPU mirror of `shaders/include/rt_light_contract.hlsli`.
//!
//! The numbers here are the reference for the HLSL: same formulas, same clamps. Numeric
//! tests live at the bottom.

pub const PI: f32 = std::f32::consts::PI;
pub const INV_PI: f32 = 1.0 / PI;
pub const EPS: f32 = 1e-6;

pub type Vec3 = [f32; 3];

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: Vec3) -> Vec3 {
    let len = dot(v, v).sqrt().max(EPS);
    [v[0] / len, v[1] / len, v[2] / len]
}

fn saturate(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

pub fn pcg_hash(v: u32) -> u32 {
    let state = v.wrapping_mul(747796405).wrapping_add(2891336453);
    let word = ((state >> ((state >> 28) + 4)) ^ state).wrapping_mul(277803737);
    (word >> 22) ^ word
}

pub fn seed(pixel: (u32, u32), frame: u32, bounce: u32, sample: u32, seed: u32) -> u32 {
    let mut h = pcg_hash(pixel.0 ^ pcg_hash(pixel.1));
    h = pcg_hash(h ^ frame.wrapping_mul(0x9E37_79B9));
    h = pcg_hash(h ^ bounce.wrapping_mul(0x85EB_CA6B));
    h = pcg_hash(h ^ sample.wrapping_mul(0xC2B2_AE35));
    pcg_hash(h ^ seed)
}

pub struct Rng {
    pub state: u32,
}

impl Rng {
    pub fn new(state: u32) -> Self {
        Self { state }
    }

    pub fn next(&mut self) -> f32 {
        self.state = self.state.wrapping_mul(747796405).wrapping_add(2891336453);
        let word = ((self.state >> ((self.state >> 28) + 4)) ^ self.state).wrapping_mul(277803737);
        (((word >> 22) ^ word) as f32) * (1.0 / 4_294_967_296.0)
    }
}

pub fn tangent_frame(n: Vec3) -> (Vec3, Vec3) {
    let up = if n[2].abs() < 0.999 {
        [0.0, 0.0, 1.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let t = normalize(cross(up, n));
    let b = cross(n, t);
    (t, b)
}

pub fn cosine_hemisphere(u: (f32, f32), n: Vec3) -> Vec3 {
    let r = u.0.sqrt();
    let phi = 2.0 * PI * u.1;
    let (t, b) = tangent_frame(n);
    let mut out = [0.0; 3];
    for i in 0..3 {
        out[i] = r * phi.cos() * t[i] + r * phi.sin() * b[i] + saturate(1.0 - u.0).sqrt() * n[i];
    }
    normalize(out)
}

pub fn cosine_pdf(cos_theta: f32) -> f32 {
    cos_theta.max(0.0) * INV_PI
}

pub fn ggx_d(n: Vec3, h: Vec3, alpha: f32) -> f32 {
    let a2 = alpha * alpha;
    let nh = dot(n, h).max(0.0);
    let d = nh * nh * (a2 - 1.0) + 1.0;
    a2 / (PI * d * d).max(EPS)
}

pub fn ggx_g1(cos_theta: f32, alpha: f32) -> f32 {
    let k = alpha * 0.5;
    cos_theta.max(0.0) / (cos_theta * (1.0 - k) + k).max(EPS)
}

pub fn ggx_pdf(n: Vec3, v: Vec3, h: Vec3, alpha: f32) -> f32 {
    let nv = dot(n, v).max(EPS);
    ggx_d(n, h, alpha) * ggx_g1(nv, alpha) / (4.0 * nv)
}

pub fn sample_ggx_vndf(u: (f32, f32), n: Vec3, v: Vec3, alpha: f32) -> (Vec3, Vec3) {
    let (t, b) = tangent_frame(n);
    let v_local = [dot(v, t), dot(v, b), dot(v, n)];
    let vh = normalize([alpha * v_local[0], alpha * v_local[1], v_local[2]]);
    let lensq = vh[0] * vh[0] + vh[1] * vh[1];
    let t1 = if lensq > 0.0 {
        let inv = 1.0 / lensq.sqrt();
        [-vh[1] * inv, vh[0] * inv, 0.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let t2 = cross(vh, t1);
    let r = u.0.sqrt();
    let phi = 2.0 * PI * u.1;
    let p1 = r * phi.cos();
    let mut p2 = r * phi.sin();
    let s = 0.5 * (1.0 + vh[2]);
    p2 = (1.0 - s) * saturate(1.0 - p1 * p1).sqrt() + s * p2;
    let pz = saturate(1.0 - p1 * p1 - p2 * p2).sqrt();
    let nh = [
        alpha * (p1 * t1[0] + p2 * t2[0] + pz * vh[0]),
        alpha * (p1 * t1[1] + p2 * t2[1] + pz * vh[1]),
        (p1 * t1[2] + p2 * t2[2] + pz * vh[2]).max(EPS),
    ];
    let h = normalize([
        t[0] * normalize(nh)[0] + b[0] * normalize(nh)[1] + n[0] * normalize(nh)[2],
        t[1] * normalize(nh)[0] + b[1] * normalize(nh)[1] + n[1] * normalize(nh)[2],
        t[2] * normalize(nh)[0] + b[2] * normalize(nh)[1] + n[2] * normalize(nh)[2],
    ]);
    let mut wi = [0.0; 3];
    for i in 0..3 {
        wi[i] = 2.0 * dot(v, h) * h[i] - v[i];
    }
    (normalize(wi), h)
}

pub fn fresnel_schlick(cos_theta: f32, f0: Vec3) -> Vec3 {
    let m = saturate(1.0 - cos_theta).powi(5);
    [
        f0[0] + (1.0 - f0[0]) * m,
        f0[1] + (1.0 - f0[1]) * m,
        f0[2] + (1.0 - f0[2]) * m,
    ]
}

pub fn ray_bias(hit_distance: f32) -> f32 {
    1e-3 + 0.01 * hit_distance
}

pub fn roulette_survival(throughput: Vec3, u: f32) -> Option<f32> {
    let q = saturate(throughput[0].max(throughput[1].max(throughput[2])));
    if q >= 1.0 {
        return Some(1.0);
    }
    if u >= q {
        return None;
    }
    Some(1.0 / q.max(EPS))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(state: u32) -> Rng {
        Rng::new(state)
    }

    #[test]
    fn cosine_pdf_integrates_to_one() {
        let n = [0.0, 0.0, 1.0];
        let mut r = rng(0x1234_5678);
        let samples = 200_000;
        let mut acc = 0.0f64;
        for _ in 0..samples {
            let z = 2.0 * r.next() - 1.0;
            let phi = 2.0 * PI * r.next();
            let s = (1.0 - z * z).max(0.0).sqrt();
            let d = [s * phi.cos(), s * phi.sin(), z];
            acc += cosine_pdf(dot(n, d)) as f64 / (1.0 / (4.0 * std::f64::consts::PI));
        }
        let integral = acc / samples as f64;
        assert!((integral - 1.0).abs() < 0.01, "integral = {integral}");
    }

    #[test]
    fn ggx_vndf_pdf_integrates_to_one() {
        let n = [0.0, 0.0, 1.0];
        let v = normalize([0.3, 0.1, 0.9]);
        let alpha = 0.4;
        let mut r = rng(0x0BAD_F00D);
        let samples = 200_000;
        let mut acc = 0.0f64;
        for _ in 0..samples {
            let z = 2.0 * r.next() - 1.0;
            let phi = 2.0 * PI * r.next();
            let s = (1.0 - z * z).max(0.0).sqrt();
            let wi = normalize([s * phi.cos(), s * phi.sin(), z]);
            let h = normalize([v[0] + wi[0], v[1] + wi[1], v[2] + wi[2]]);
            if dot(v, h) <= 0.0 {
                continue;
            }
            acc += ggx_pdf(n, v, h, alpha) as f64 / (1.0 / (4.0 * std::f64::consts::PI));
        }
        let integral = acc / samples as f64;
        assert!((integral - 1.0).abs() < 0.05, "integral = {integral}");
    }

    #[test]
    fn roulette_is_unbiased() {
        let throughput = [0.2, 0.1, 0.05];
        let mut r = rng(0xFEED_BEEF);
        let trials = 200_000;
        let mut acc = 0.0f64;
        for _ in 0..trials {
            if let Some(w) = roulette_survival(throughput, r.next()) {
                acc += w as f64;
            }
        }
        let mean = acc / trials as f64;
        assert!((mean - 1.0).abs() < 0.02, "mean = {mean}");
    }

    #[test]
    fn cosine_samples_stay_on_hemisphere() {
        let n = normalize([0.4, -0.2, 0.9]);
        let mut r = rng(0x5EED);
        for _ in 0..10_000 {
            let d = cosine_hemisphere((r.next(), r.next()), n);
            assert!(dot(d, n) > -1e-4, "sample behind the surface");
        }
    }
}
