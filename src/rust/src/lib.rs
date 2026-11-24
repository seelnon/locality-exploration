use std::cell::UnsafeCell;
use std::f32::consts::PI;

const MAX_PARTICLES: usize = 50_000;
const MAX_KNOTS: usize = 128;
const MAX_BONDS: usize = 128;
const MAX_SHOCKWAVES: usize = 32;
const MAX_GRID_NODES: usize = 12_800;
const HASH_BUCKETS: usize = 4_096;
const KNOT_STRIDE: usize = 16;
const KNOT_R_LO: f32 = 44.0;
const KNOT_R_HI: f32 = 70.0;
const FAR_MAX_NODES: usize = 2_400;
const KNOT_HASH_MAX: usize = 512;

#[inline]
fn fast_sin(x: f32) -> f32 {
    const INV_TWO_PI: f32 = 0.159154943;
    let k = (x * INV_TWO_PI).round();
    let r = x - k * (2.0 * PI);
    let y = r * (1.27323954 - 0.405284735 * r.abs());
    0.225 * (y * y.abs() - y) + y
}

#[inline]
fn fast_cos(x: f32) -> f32 {
    fast_sin(x + PI * 0.5)
}

#[inline]
fn fast_hash(mut x: u32) -> f32 {
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
    x = (x >> 16) ^ x;
    (x & 0x00FF_FFFF) as f32 / 16777216.0
}

#[inline]
fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    if !v.is_finite() {
        return if v.is_infinite() && !v.is_sign_negative() { hi } else { lo };
    }
    if v < lo { lo } else if v > hi { hi } else { v }
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Shortest signed displacement on a tiled [0, span) toroidal axis.
#[inline]
fn torus_delta(mut dx: f32, span: f32) -> f32 {
    if span <= 0.0 || !dx.is_finite() {
        return 0.0;
    }
    let half = span * 0.5;
    if dx > half || dx < -half {
        dx -= (dx / span).round() * span;
    }
    dx
}

/// Planck-scale mass grip saturating from free waves toward 1.0 as mass crosses M_P.
#[inline]
fn knot_grip(mass: f32, planck_mass: f32) -> f32 {
    let m = mass.max(0.0);
    let mp = planck_mass.max(0.1);
    let m3 = m * m * m;
    let mp3 = mp * mp * mp;
    let s = m3 / (m3 + mp3).max(1e-6);
    1.0 - (1.0 - 0.60) * (1.0 - s)
}

/// Physical mass-energy color ramp harmonized with Dark Coffee + Elden Ring Altus Gold:
/// Low mass/energy -> Deep Verdigris | Nominal -> Burnished Gold | High energy / polar jet -> Molten Ember
#[inline]
fn mass_energy_color(rel_mass: f32) -> (f32, f32, f32) {
    let u = clamp((rel_mass - 0.50) / 1.75, 0.0, 1.0);
    if u < 0.35 {
        let t = u / 0.35;
        (lerp(0.30, 0.62, t), lerp(0.59, 0.65, t), lerp(0.61, 0.43, t))
    } else if u < 0.68 {
        let t = (u - 0.35) / 0.33;
        (lerp(0.62, 0.85, t), lerp(0.65, 0.69, t), lerp(0.43, 0.23, t))
    } else {
        let t = (u - 0.68) / 0.32;
        (lerp(0.85, 0.95, t), lerp(0.69, 0.28, t), lerp(0.23, 0.19, t))
    }
}

/// Dynamically-warped spacetime horizon radius & angular metrics.
/// Deforms with spin oblateness, rotating 3-strand helical braid cos(3θ - φ),
/// companion tidal quadrupole elongation, and local 2D wave-sheet curvature.
#[inline]
fn warped_horizon(
    base_r: f32,
    ux: f32,
    uy: f32,
    rx_u: f32,
    ry_u: f32,
    angular_vel: f32,
    bp_cos: f32,
    bp_sin: f32,
    tidal_pull: f32,
    tidal_dx: f32,
    tidal_dy: f32,
    u_loc: f32,
) -> (f32, f32, f32) {
    let cos_th = clamp(rx_u * ux + ry_u * uy, -1.0, 1.0);
    let sin_signed = clamp(rx_u * (-uy) + ry_u * ux, -1.0, 1.0);
    let sin_th = sin_signed.abs();

    let spin_k = clamp(angular_vel.abs() * 0.045, 0.0, 0.22);
    let oblate = 1.0 + spin_k * (1.0 - 2.0 * cos_th * cos_th);

    let cos3 = cos_th * (4.0 * cos_th * cos_th - 3.0);
    let sin3 = sin_signed * (3.0 - 4.0 * sin_signed * sin_signed);
    let braid = 1.0 + 0.095 * (cos3 * bp_cos + sin3 * bp_sin);

    let proj = if tidal_pull > 0.01 { rx_u * tidal_dx + ry_u * tidal_dy } else { 0.0 };
    let tidal = 1.0 + clamp(tidal_pull, 0.0, 1.2) * 0.28 * (2.0 * proj * proj - 1.0);

    let wave = 1.0 + clamp(-u_loc * 0.04, -0.20, 0.24);
    let r_eff = base_r * clamp(oblate * braid * tidal * wave, 0.58, 1.55);
    (r_eff, sin_th, proj)
}

#[inline]
fn smooth_pole_gate(sin_th: f32, sin_psi: f32) -> f32 {
    let width = 0.14 + sin_psi * 0.22;
    let base = clamp(0.5 - 0.5 * ((sin_th - sin_psi) / width), 0.0, 1.0);
    let full_open = clamp((sin_psi - 0.85) / 0.15, 0.0, 1.0);
    lerp(base, 1.0, full_open)
}

/// Bilson-Thompson / Markopoulou / Smolin 3-strand helon twist charge:
/// q = (t0 + t1 + t2) / 3, where t_i in {-1, 0, +1}.
#[inline]
fn braid_charge(twists: [i8; 3]) -> f32 {
    (twists[0] as i32 + twists[1] as i32 + twists[2] as i32) as f32 / 3.0
}

#[inline]
fn twists_from_thirds(thirds: i32) -> [i8; 3] {
    match thirds.clamp(-3, 3) {
        -3 => [-1, -1, -1],
        -2 => [-1, -1, 0],
        -1 => [-1, 0, 0],
        1 => [1, 0, 0],
        2 => [1, 1, 0],
        3 => [1, 1, 1],
        _ => [0, 0, 0],
    }
}

#[derive(Clone, Copy)]
struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    prev_x: f32,
    prev_y: f32,
    phase: f32,
    freq_mult: f32,
    base_freq_mult: f32,
    twist_thirds: i8,
    charge: f32,
    size: f32,
    r: f32,
    g: f32,
    b: f32,
    trapped_in: i32,
    polar_jet_age: f32,
    dent: f32,
}

impl Default for Particle {
    fn default() -> Self {
        Self {
            x: 0.0, y: 0.0, vx: 0.0, vy: 0.0, prev_x: 0.0, prev_y: 0.0,
            phase: 0.0, freq_mult: 1.0, base_freq_mult: 1.0, twist_thirds: 0, charge: 0.0, size: 1.6,
            r: 0.63, g: 0.54, b: 0.37,
            trapped_in: -1, polar_jet_age: 0.0, dent: 0.0,
        }
    }
}

#[derive(Clone, Copy)]
struct Knot {
    #[allow(dead_code)]
    id: u32,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    mass: f32,
    radius: f32,
    cloak_radius: f32,
    strand_twists: [i8; 3],
    charge: f32,
    angular_vel: f32,
    axis_angle: f32,
    braid_phase: f32,
    spew_acc: f32,
    folding_progress: f32,
    is_forming: bool,
    active: bool,
    is_quark: bool,
    color_vx: f32,
    color_vy: f32,
    trapped_cap_bonus: i32,
    e_core: f32,
    e_trapped: f32,
    axis_tilt_cb: f32,
    spin_prec_w: f32,
    grip: f32,
}

impl Default for Knot {
    fn default() -> Self {
        Self {
            id: 0, x: 0.0, y: 0.0, vx: 0.0, vy: 0.0,
            mass: 0.0, radius: 8.0, cloak_radius: 14.0,
            strand_twists: [0, 0, 0], charge: 0.0, angular_vel: 1.0,
            axis_angle: PI * 0.5, braid_phase: 0.0, spew_acc: 0.0,
            folding_progress: 0.0, is_forming: true, active: false,
            is_quark: false, color_vx: 0.0, color_vy: 0.0,
            trapped_cap_bonus: 0, e_core: 0.0, e_trapped: 0.0,
            axis_tilt_cb: 0.55, spin_prec_w: 0.0, grip: 0.60,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct QuarkBond {
    a: usize,
    b: usize,
    active: bool,
    tension: f32,
}

#[derive(Clone, Copy, Default)]
struct Shockwave {
    x: f32,
    y: f32,
    radius: f32,
    max_radius: f32,
    #[allow(dead_code)]
    energy: f32,
    kinetic_push: f32,
    amp: f32,
    speed: f32,
    age: f32,
    max_age: f32,
    active: bool,
}

#[derive(Clone, Copy, Default)]
struct ActiveKnot {
    idx: usize,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    mass: f32,
    radius: f32,
    cloak_radius: f32,
    angular_vel: f32,
    axis_tilt_cb: f32,
    braid_phase: f32,
    bp_cos: f32,
    bp_sin: f32,
    tidal_pull: f32,
    tidal_dx: f32,
    tidal_dy: f32,
    mx: f32,
    my: f32,
    ux: f32,
    uy: f32,
    mu_k: f32,
    grip: f32,
    f_scale: f32,
    folding_progress: f32,
    is_quark: bool,
    j_spin_drive: f32,
    start_d: f32,
    end_d: f32,
    end_d2: f32,
    braid_gain: f32,
    handedness: f32,
}

#[derive(Clone, Copy, Default)]
struct FarFieldNode {
    gx: f32,
    gy: f32,
    vdamp: f32,
    bx: f32,
    by: f32,
    closeness: f32,
}

#[derive(Clone, Copy, Default)]
struct GridNode {
    ox: f32,
    oy: f32,
    x: f32,
    y: f32,
    phi: f32,
    ax: f32,
    ay: f32,
    ex: f32,
    ey: f32,
    bz: f32,
    rho: f32,
    jx: f32,
    jy: f32,
    strain: f32,
    u: f32,
    v: f32,
    f_load: f32,
    edge_damp: f32,
    slope: f32,
    num_dens: f32,
    mass_dens: f32,
}

struct SimState {
    width: f32,
    height: f32,
    time: f32,
    rng_seed: u32,
    next_knot_id: u32,

    // Parameters synced from JS / math.js
    medium_id: u8,         // 0=A (filament), 1=B (volumetric vorticity), 2=C (relativistic vacuum)
    pop_style: u8,         // 0=default, 1=orbital, 2=chaos, 3=spiral, 4=grid, 5=central
    particle_count: usize,
    frequency: f32,
    c: f32,
    h: f32,
    gravity: f32,          // Scaled G (e.g., 6.6743; 0 disables gravity & folding)
    drag: f32,
    sim_speed: f32,
    dark_energy: f32,
    orbital_boost: f32,
    gravity_range: f32,
    initial_spin: f32,
    center_bias: f32,
    sector_size: f32,
    em_coupling: f32,
    sheet_tension: f32,
    pole_gate_deg: f32,

    // Toggles
    individual_pull: bool,
    intra_collisions: bool,
    non_destructible: bool,
    permanent_clumps: bool,
    trapped_mode: u8,      // 0=prob (pole gates), 1=always (one-way capture), 2=off
    em_color_mode: bool,   // Show charge polarity colors only in EM_FIELD view mode
    render_mode: u8,       // 0=physics, 1=grid, 2=fluid, 3=vector

    // Interactive mouse attractor
    attr_active: bool,
    attr_x: f32,
    attr_y: f32,
    attr_raw_x: f32,
    attr_raw_y: f32,
    attr_strength: f32,

    // Derived physical constants
    base_mass: f32,
    planck_mass: f32,
    spacetime_tension: f32,

    // Entities
    particles: Vec<Particle>,
    knots: Vec<Knot>,
    bonds: Vec<QuarkBond>,
    shockwaves: Vec<Shockwave>,

    // 2D Metric & Wave Grid
    grid_cols: usize,
    grid_rows: usize,
    grid_spacing: f32,
    grid_dx: f32,
    grid_dy: f32,
    grid: Vec<GridNode>,
    smooth_tmp: Vec<f32>,

    // Spatial hash & sector accumulators
    head: Vec<i32>,
    next: Vec<i32>,
    cell_mass: Vec<f32>,
    cell_x: Vec<f32>,
    cell_y: Vec<f32>,
    cell_vx: Vec<f32>,
    cell_vy: Vec<f32>,
    cell_vort: Vec<f32>,
    cell_dent: Vec<f32>,
    cell_count: Vec<i32>,

    // P3M Knot Acceleration & Tidal Merger Buffers
    ak_buf: Vec<ActiveKnot>,
    ak_count: usize,
    far_grid: Vec<FarFieldNode>,
    far_cols: usize,
    far_rows: usize,
    far_dx: f32,
    far_dy: f32,
    use_far_field: bool,
    k_cell_head: Vec<i32>,
    k_cell_next: Vec<i32>,
    k_cols: usize,
    k_rows: usize,
    k_cell_w: f32,
    k_cell_h: f32,
    tidal_dx: Vec<f32>,
    tidal_dy: Vec<f32>,
    tidal_pull: Vec<f32>,
    chirp_pulses: [(f32, f32, f32, f32); 32],
    chirp_count: usize,
    spin_acc: Vec<f32>,

    // Flat output buffers for WebGL
    render_particles: Vec<f32>,  // [x, y, r, g, b, a, size] * N
    render_trails: Vec<f32>,     // [px, py, x, y, r, g, b, a] * N
    render_grid: Vec<f32>,       // [x, y, ex, ey, bz, strain, u, slope] * grid_len
    render_knots: Vec<f32>,      // 16 floats * MAX_KNOTS
    render_bonds: Vec<f32>,      // [x1, y1, x2, y2, r, g, b, a] * MAX_BONDS
    render_shockwaves: Vec<f32>, // [x, y, radius, alpha] * MAX_SHOCKWAVES
    metrics: [f32; 8],
}

struct SyncSim(UnsafeCell<Option<SimState>>);
unsafe impl Sync for SyncSim {}

static SIM: SyncSim = SyncSim(UnsafeCell::new(None));

#[inline]
fn get_sim() -> &'static mut SimState {
    unsafe {
        let slot = &mut *SIM.0.get();
        if slot.is_none() {
            *slot = Some(SimState::new());
        }
        slot.as_mut().unwrap()
    }
}

impl SimState {
    fn new() -> Self {
        Self {
            width: 540.0,
            height: 380.0,
            time: 0.0,
            rng_seed: 1337,
            next_knot_id: 1,
            medium_id: 0,
            pop_style: 4,
            particle_count: 3000,
            frequency: 2.4,
            c: 3.0,
            h: 6.62607015,
            gravity: 6.6743,
            drag: 0.0,
            sim_speed: 1.0,
            dark_energy: 0.0,
            orbital_boost: 50.0,
            gravity_range: 70.0,
            initial_spin: 1.0,
            center_bias: 0.0,
            sector_size: 70.0,
            em_coupling: 1.0,
            sheet_tension: 0.22,
            pole_gate_deg: 30.0,
            individual_pull: true,
            intra_collisions: true,
            non_destructible: true,
            permanent_clumps: true,
            trapped_mode: 0,
            em_color_mode: false,
            render_mode: 1,
            attr_active: false,
            attr_x: 0.0,
            attr_y: 0.0,
            attr_raw_x: 0.0,
            attr_raw_y: 0.0,
            attr_strength: 5.0,
            base_mass: 0.736,
            planck_mass: 17.2,
            spacetime_tension: 6.0,
            particles: vec![Particle::default(); MAX_PARTICLES],
            knots: vec![Knot::default(); MAX_KNOTS],
            bonds: vec![QuarkBond::default(); MAX_BONDS],
            shockwaves: vec![Shockwave::default(); MAX_SHOCKWAVES],
            grid_cols: 0,
            grid_rows: 0,
            grid_spacing: 10.0,
            grid_dx: 10.0,
            grid_dy: 10.0,
            grid: vec![GridNode::default(); MAX_GRID_NODES],
            smooth_tmp: vec![0.0; MAX_GRID_NODES],
            head: vec![-1; HASH_BUCKETS],
            next: vec![-1; MAX_PARTICLES],
            cell_mass: vec![0.0; 512],
            cell_x: vec![0.0; 512],
            cell_y: vec![0.0; 512],
            cell_vx: vec![0.0; 512],
            cell_vy: vec![0.0; 512],
            cell_vort: vec![0.0; 512],
            cell_dent: vec![0.0; 512],
            cell_count: vec![0; 512],
            ak_buf: vec![ActiveKnot::default(); MAX_KNOTS],
            ak_count: 0,
            far_grid: vec![FarFieldNode::default(); FAR_MAX_NODES],
            far_cols: 0,
            far_rows: 0,
            far_dx: 30.0,
            far_dy: 30.0,
            use_far_field: false,
            k_cell_head: vec![-1; KNOT_HASH_MAX],
            k_cell_next: vec![-1; MAX_KNOTS],
            k_cols: 4,
            k_rows: 4,
            k_cell_w: 88.0,
            k_cell_h: 88.0,
            tidal_dx: vec![0.0; MAX_KNOTS],
            tidal_dy: vec![0.0; MAX_KNOTS],
            tidal_pull: vec![0.0; MAX_KNOTS],
            chirp_pulses: [(0.0, 0.0, 0.0, 0.0); 32],
            chirp_count: 0,
            spin_acc: vec![0.0; MAX_KNOTS],
            render_particles: vec![0.0; MAX_PARTICLES * 7],
            render_trails: vec![0.0; MAX_PARTICLES * 8],
            render_grid: vec![0.0; MAX_GRID_NODES * 8],
            render_knots: vec![0.0; MAX_KNOTS * KNOT_STRIDE],
            render_bonds: vec![0.0; MAX_BONDS * 8],
            render_shockwaves: vec![0.0; MAX_SHOCKWAVES * 4],
            metrics: [0.0; 8],
        }
    }

    #[inline]
    fn rand(&mut self) -> f32 {
        self.rng_seed = self.rng_seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (self.rng_seed >> 8) as f32 / 16777216.0
    }

    fn update_derived_constants(&mut self) {
        let c2 = (self.c * self.c).max(0.01);
        self.base_mass = (self.frequency.max(0.0) * self.h) / c2;
        if self.gravity <= 0.0001 {
            self.planck_mass = 1e9;
            self.spacetime_tension = 1e9;
        } else {
            let g_eff = self.gravity;
            // Planck mass threshold M_P = sqrt(h*c / (2*pi*G)), calibrated so uniform calm field stays ~8x below collapse
            self.planck_mass = ((self.h * self.c) / (2.0 * PI * g_eff)).sqrt() * 25.0;
            // Hoop tension limit T_st = c^4 / (2*G)
            self.spacetime_tension = (c2 * c2) / (2.0 * g_eff);
        }
    }

    fn setup_grid(&mut self) {
        let mut spacing = 10.0f32;
        let mut cols = ((self.width / spacing).round() as usize + 1).max(4);
        let mut rows = ((self.height / spacing).round() as usize + 1).max(4);
        while cols * rows > MAX_GRID_NODES {
            spacing += 1.0;
            cols = ((self.width / spacing).round() as usize + 1).max(4);
            rows = ((self.height / spacing).round() as usize + 1).max(4);
        }
        let grid_dx = self.width / (cols - 1) as f32;
        let grid_dy = self.height / (rows - 1) as f32;
        self.grid_dx = grid_dx.max(1.0);
        self.grid_dy = grid_dy.max(1.0);
        self.grid_spacing = 0.5 * (self.grid_dx + self.grid_dy);
        self.grid_cols = cols;
        self.grid_rows = rows;
        let total = (cols * rows).min(MAX_GRID_NODES);
        for c in 0..cols {
            for r in 0..rows {
                let idx = c * rows + r;
                if idx >= total { break; }
                let ox = if c + 1 == cols { self.width } else { c as f32 * grid_dx };
                let oy = if r + 1 == rows { self.height } else { r as f32 * grid_dy };
                self.grid[idx] = GridNode {
                    ox, oy, x: ox, y: oy,
                    edge_damp: 0.015,
                    ..Default::default()
                };
                let off = idx * 8;
                self.render_grid[off] = ox;
                self.render_grid[off + 1] = oy;
                self.render_grid[off + 2] = 0.0;
                self.render_grid[off + 3] = 0.0;
                self.render_grid[off + 4] = 0.0;
                self.render_grid[off + 5] = 0.0;
                self.render_grid[off + 6] = 0.0;
                self.render_grid[off + 7] = 0.0;
            }
        }
    }

    #[inline]
    fn sample_manifold_pos(&self, x: f32, y: f32) -> (f32, f32) {
        let cols = self.grid_cols;
        let rows = self.grid_rows;
        if cols < 2 || rows < 2 {
            return (x, y);
        }
        let grid_len = (cols * rows).min(MAX_GRID_NODES);
        let fx = clamp(x / self.grid_dx.max(1.0), 0.0, (cols - 1) as f32 - 0.0001);
        let fy = clamp(y / self.grid_dy.max(1.0), 0.0, (rows - 1) as f32 - 0.0001);
        let gc = fx.floor() as usize;
        let gr = fy.floor() as usize;
        let tx = fx - gc as f32;
        let ty = fy - gr as f32;
        let i00 = gc * rows + gr;
        let i10 = (gc + 1) * rows + gr;
        let i01 = gc * rows + (gr + 1);
        let i11 = (gc + 1) * rows + (gr + 1);
        if i11 >= grid_len {
            return (x, y);
        }
        let w00 = (1.0 - tx) * (1.0 - ty);
        let w10 = tx * (1.0 - ty);
        let w01 = (1.0 - tx) * ty;
        let w11 = tx * ty;
        let mx = w00 * self.grid[i00].x + w10 * self.grid[i10].x + w01 * self.grid[i01].x + w11 * self.grid[i11].x;
        let my = w00 * self.grid[i00].y + w10 * self.grid[i10].y + w01 * self.grid[i01].y + w11 * self.grid[i11].y;
        (mx, my)
    }

    #[inline]
    fn unmap_manifold_pos(&self, vis_x: f32, vis_y: f32) -> (f32, f32) {
        let mut ux = clamp(vis_x, 0.0, self.width);
        let mut uy = clamp(vis_y, 0.0, self.height);
        for _ in 0..4 {
            let (mx, my) = self.sample_manifold_pos(ux, uy);
            ux = clamp(ux + (vis_x - mx), 0.0, self.width);
            uy = clamp(uy + (vis_y - my), 0.0, self.height);
        }
        (ux, uy)
    }

    fn init_particles(&mut self) {
        self.update_derived_constants();
        for k in self.knots.iter_mut() { k.active = false; }
        for sa in self.spin_acc.iter_mut() { *sa = 0.0; }
        for b in self.bonds.iter_mut() { b.active = false; }
        for s in self.shockwaves.iter_mut() { s.active = false; }
        for g in self.grid.iter_mut() {
            g.u = 0.0; g.v = 0.0; g.f_load = 0.0; g.slope = 0.0;
            g.phi = 0.0; g.ax = 0.0; g.ay = 0.0; g.ex = 0.0; g.ey = 0.0; g.bz = 0.0;
            g.strain = 0.0; g.x = g.ox; g.y = g.oy;
        }

        let count = self.particle_count.min(MAX_PARTICLES);
        let cx = self.width * 0.5;
        let cy = self.height * 0.5;
        let min_dim = self.width.min(self.height);
        let f_init = clamp(self.frequency.abs(), 0.0, 1.0);

        for i in 0..count {
            let r1 = self.rand();
            let r2 = self.rand();
            let r3 = self.rand();
            let angle = r1 * PI * 2.0;
            let radius = 20.0 + r2 * (min_dim * 0.42);

            let (x, y, vx, vy) = match self.pop_style {
                1 => { // Keplerian ring
                    let ring_r = min_dim * 0.28 + (r2 - 0.5) * 24.0;
                    let spd = 1.0 * f_init.max(0.25);
                    (cx + angle.cos() * ring_r, cy + angle.sin() * ring_r, -angle.sin() * spd, angle.cos() * spd)
                }
                2 => { // Thermal cloud
                    let spd = 1.1 * f_init;
                    (r1 * self.width, r2 * self.height, (r3 - 0.5) * spd, (self.rand() - 0.5) * spd)
                }
                3 => { // Spiral accretion
                    let arm = (i % 3) as f32 * ((PI * 2.0) / 3.0);
                    let dist = r2.powf(0.8) * (min_dim * 0.44);
                    let sa = arm + dist * 0.015 + (r3 - 0.5) * 0.25;
                    let spd = 1.05 * f_init.max(0.25);
                    (cx + sa.cos() * dist, cy + sa.sin() * dist, -sa.sin() * spd, sa.cos() * spd)
                }
                4 => { // Uniform lattice covering 100% of available simulation area with aspect-matched cell-centered spacing
                    let aspect = (self.width / self.height.max(1.0)).max(0.25);
                    let ideal_cols = (count as f32 * aspect).sqrt().round().max(1.0) as usize;
                    let mut cols = ideal_cols.max(1);
                    let mut rows = ((count + cols - 1) / cols).max(1);
                    let mut best_score = f32::MAX;
                    let lo_c = ideal_cols.saturating_sub(3).max(1);
                    let hi_c = (ideal_cols + 3).max(1);
                    for c_try in lo_c..=hi_c {
                        let r_try = ((count + c_try - 1) / c_try).max(1);
                        let leftover = (c_try * r_try - count) as f32 / (c_try as f32);
                        let ar_err = ((c_try as f32 / r_try as f32) / aspect - 1.0).abs();
                        let score = ar_err * 2.0 + leftover * 0.5;
                        if score < best_score {
                            best_score = score;
                            cols = c_try;
                            rows = r_try;
                        }
                    }
                    let col = i % cols;
                    let row = i / cols;
                    let sx = self.width / cols as f32;
                    let sy = self.height / rows as f32;
                    let x_off = if row == rows - 1 {
                        let in_row = count - (rows - 1) * cols;
                        if in_row < cols { 0.5 * (cols - in_row) as f32 * sx } else { 0.0 }
                    } else {
                        0.0
                    };
                    let jx = (r1 - 0.5) * 0.03 * f_init;
                    let jy = (r2 - 0.5) * 0.03 * f_init;
                    ((col as f32 + 0.5) * sx + x_off, (row as f32 + 0.5) * sy, jx, jy)
                }
                5 => { // Central singularity
                    let cr = r2.powf(2.5) * 75.0;
                    let spd = 0.45 * f_init.max(0.2);
                    (cx + angle.cos() * cr, cy + angle.sin() * cr, -angle.sin() * spd, angle.cos() * spd)
                }
                _ => { // Metric default
                    match self.medium_id {
                        0 => {
                            let track = ((i % 4) as f32) * (PI * 0.5);
                            let d = r2 * min_dim * 0.44;
                            (
                                cx + track.cos() * d + (r3 - 0.5) * 10.0,
                                cy + track.sin() * d + (self.rand() - 0.5) * 10.0,
                                -track.sin() * 0.25 * f_init,
                                track.cos() * 0.25 * f_init,
                            )
                        }
                        1 => {
                            let rb = 28.0 + r2 * (min_dim * 0.40);
                            (cx + angle.cos() * rb, cy + angle.sin() * rb, -angle.sin() * 1.1 * f_init, angle.cos() * 1.1 * f_init)
                        }
                        _ => {
                            (cx + angle.cos() * radius, cy + angle.sin() * radius, -angle.sin() * 0.55 * f_init, angle.cos() * 0.55 * f_init)
                        }
                    }
                }
            };

            let freq_mult = 0.55 + 1.00 * r2;
            let (cr, cg, cb) = mass_energy_color(freq_mult);
            let p_size = 0.85 + (freq_mult - 0.55) * 0.85 + r3 * 0.20;

            self.particles[i] = Particle {
                x, y, vx, vy, prev_x: x, prev_y: y,
                phase: r1 * PI * 2.0,
                freq_mult,
                base_freq_mult: freq_mult,
                twist_thirds: 0,
                charge: 0.0,
                size: p_size,
                r: cr, g: cg, b: cb,
                trapped_in: -1,
                polar_jet_age: 0.0,
                dent: 0.0,
            };
        }
        self.setup_grid();
        if self.render_mode == 1 {
            self.step_metric_grid(0.0);
        }
        let v_c = (self.c * 4.0).max(0.5);
        self.pack_outputs(v_c * v_c);
    }

    fn spawn_shockwave(
        &mut self,
        x: f32,
        y: f32,
        radius: f32,
        max_radius: f32,
        energy: f32,
        kinetic_push: f32,
        amp: f32,
        speed: f32,
    ) {
        for sw in self.shockwaves.iter_mut() {
            if !sw.active {
                *sw = Shockwave {
                    x, y, radius, max_radius, energy, kinetic_push, amp, speed,
                    age: 0.0, max_age: 68.0, active: true,
                };
                return;
            }
        }
    }

    fn explode(&mut self) {
        let count = self.particle_count.min(MAX_PARTICLES);
        for i in 0..count {
            let p = &mut self.particles[i];
            p.trapped_in = -1;
            let a = fast_hash(i as u32 ^ self.rng_seed) * PI * 2.0;
            let s = 2.8 + fast_hash((i as u32).wrapping_mul(7)) * 6.5;
            p.vx += a.cos() * s;
            p.vy += a.sin() * s;
        }
        self.spawn_shockwave(self.width * 0.5, self.height * 0.5, 6.0, 220.0, 12.0, 12.0, 0.55, 3.6);
        self.rng_seed = self.rng_seed.wrapping_add(97);
    }

    /// Conserves total topological third-twists across the field whenever a knot dissolves.
    fn return_charge_to_field(&mut self, mut rem_thirds: i32) {
        if rem_thirds == 0 { return; }
        let count = self.particle_count.min(MAX_PARTICLES);
        for i in 0..count {
            let p = &mut self.particles[i];
            let new_t = (p.twist_thirds as i32 + rem_thirds).clamp(-3, 3);
            let used = new_t - p.twist_thirds as i32;
            if used != 0 {
                p.twist_thirds = new_t as i8;
                p.charge = new_t as f32 / 3.0;
                rem_thirds -= used;
                if rem_thirds == 0 { break; }
            }
        }
    }

    fn step_wave_sheet(&mut self) {
        let cols = self.grid_cols;
        let rows = self.grid_rows;
        let grid_len = (cols * rows).min(MAX_GRID_NODES);
        if cols <= 2 || rows <= 2 { return; }
        let cols_u = cols - 1;
        let rows_u = rows - 1;

        let dx_g = self.grid_dx.max(1.0);
        let dy_g = self.grid_dy.max(1.0);
        let spacing = self.grid_spacing.max(1.0);
        let count = self.particle_count.min(MAX_PARTICLES);
        let g_norm = if self.gravity <= 0.0001 { 0.0 } else { self.gravity / 6.6743 };
        let freq_pos = self.frequency.max(0.0);

        for g in self.grid[..grid_len].iter_mut() {
            g.f_load = 0.0;
        }

        // Particles only dent the spacetime sheet proportionally to ACTUAL frequency & gravity!
        // Zero-frequency / zero-gravity leaves f_load = 0 (strictly flat & calm!).
        if g_norm > 0.0 && freq_pos > 0.0 {
            let n_norm = ((count as f32 / 350.0).max(1.0)).sqrt();
            let g_wave = (0.024 * g_norm.clamp(0.0, 4.0)) / n_norm;
            let f_scale = (freq_pos / 8.0).clamp(0.0, 2.2);
            // Mean dent weight scales smoothly with frequency while oscillation creates interference ripples
            let dent_bias = clamp(freq_pos / 18.0, 0.05, 0.70);

            for i in 0..count {
                let p = &self.particles[i];
                if p.trapped_in >= 0 { continue; }
                let fx = clamp(p.x / dx_g, 0.0, cols_u as f32 - 0.0001);
                let fy = clamp(p.y / dy_g, 0.0, rows_u as f32 - 0.0001);
                let gc = fx.floor() as usize;
                let gr = fy.floor() as usize;
                let c1 = if gc + 1 >= cols_u { 0 } else { gc + 1 };
                let r1 = if gr + 1 >= rows_u { 0 } else { gr + 1 };
                let tx = fx - gc as f32;
                let ty = fy - gr as f32;
                let osc = dent_bias + 0.72 * p.phase.sin();
                let amp = g_wave * p.freq_mult * f_scale * osc;
                let i00 = gc * rows + gr;
                let i10 = c1 * rows + gr;
                let i01 = gc * rows + r1;
                let i11 = c1 * rows + r1;
                if i00 < grid_len && i10 < grid_len && i01 < grid_len && i11 < grid_len {
                    self.grid[i00].f_load -= amp * (1.0 - tx) * (1.0 - ty);
                    self.grid[i10].f_load -= amp * tx * (1.0 - ty);
                    self.grid[i01].f_load -= amp * (1.0 - tx) * ty;
                    self.grid[i11].f_load -= amp * tx * ty;
                }
            }
        }

        // Sealed/forming knots depress the wave sheet proportional to E = M c^2 * G (periodic torus)
        if g_norm > 0.0 {
            for k in self.knots.iter() {
                if !k.active { continue; }
                let rk = clamp(k.cloak_radius / spacing, 2.0, 6.5);
                let rk2 = rk * rk;
                let ri = rk.ceil() as i32;
                let mass_ratio = (k.mass / self.planck_mass.max(0.01)).clamp(0.35, 3.5);
                let amp = (0.055 * g_norm.clamp(0.0, 4.0) * mass_ratio * k.folding_progress) / (rk2 * 0.35);

                let gc = (k.x / dx_g).round() as i32;
                let gr = (k.y / dy_g).round() as i32;
                for dc in -ri..=ri {
                    let nc = (gc + dc).rem_euclid(cols_u as i32) as usize;
                    for dr in -ri..=ri {
                        let d2 = (dc * dc + dr * dr) as f32;
                        if d2 < rk2 {
                            let nr = (gr + dr).rem_euclid(rows_u as i32) as usize;
                            let nidx = nc * rows + nr;
                            if nidx < grid_len {
                                self.grid[nidx].f_load -= amp * (1.0 - d2 / rk2);
                            }
                        }
                    }
                }
            }
        }

        // Propagating shockwave rings (periodic torus)
        for sw in self.shockwaves.iter() {
            if !sw.active { continue; }
            let age_fac = 1.0 - sw.age / sw.max_age.max(1.0);
            let r_grid = sw.radius / spacing;
            let band_g = 2.6f32;
            let ri = (r_grid + band_g).ceil() as i32;
            let gc = (sw.x / dx_g).round() as i32;
            let gr = (sw.y / dy_g).round() as i32;
            for dc in -ri..=ri {
                let nc = (gc + dc).rem_euclid(cols_u as i32) as usize;
                for dr in -ri..=ri {
                    let dist_g = ((dc * dc + dr * dr) as f32).sqrt();
                    let delta = dist_g - r_grid;
                    if delta.abs() < band_g {
                        let nr = (gr + dr).rem_euclid(rows_u as i32) as usize;
                        let nidx = nc * rows + nr;
                        if nidx < grid_len {
                            let ring_wave = (-((delta / 1.1).powi(2))).exp() * (delta * 0.9).sin();
                            self.grid[nidx].f_load += ring_wave * sw.amp * age_fac;
                        }
                    }
                }
            }
        }

        // Quadrupolar gravitational-wave chirp & ringdown ripples from inspiraling / coalescing knots
        for cp in 0..self.chirp_count {
            let (cx_w, cy_w, strength, axis_ang) = self.chirp_pulses[cp];
            let gc = (cx_w / dx_g).round() as i32;
            let gr = (cy_w / dy_g).round() as i32;
            let c2a = (2.0 * axis_ang).cos();
            let s2a = (2.0 * axis_ang).sin();
            for dc in -4i32..=4 {
                let nc = (gc + dc).rem_euclid(cols_u as i32) as usize;
                for dr in -4i32..=4 {
                    let d2 = (dc * dc + dr * dr) as f32;
                    if d2 > 0.25 && d2 <= 18.0 {
                        let nr = (gr + dr).rem_euclid(rows_u as i32) as usize;
                        let nidx = nc * rows + nr;
                        if nidx < grid_len {
                            let inv_d2 = 1.0 / d2;
                            let cos2t = ((dc * dc - dr * dr) as f32) * inv_d2;
                            let sin2t = (2.0 * (dc * dr) as f32) * inv_d2;
                            let quad = cos2t * c2a + sin2t * s2a;
                            let env = (-d2 / 7.5).exp();
                            self.grid[nidx].f_load += quad * env * strength;
                        }
                    }
                }
            }
        }

        // Interactive mouse pointer dent
        if self.attr_active {
            let gc = (self.attr_x / dx_g).round() as i32;
            let gr = (self.attr_y / dy_g).round() as i32;
            let rk = 4.2f32;
            for dc in -4i32..=4 {
                let nc = (gc + dc).rem_euclid(cols_u as i32) as usize;
                for dr in -4i32..=4 {
                    let d2 = (dc * dc + dr * dr) as f32;
                    if d2 < rk * rk {
                        let nr = (gr + dr).rem_euclid(rows_u as i32) as usize;
                        let nidx = nc * rows + nr;
                        if nidx < grid_len {
                            self.grid[nidx].f_load -= 0.40 * self.attr_strength * 0.25 * (1.0 - d2 / (rk * rk));
                        }
                    }
                }
            }
        }

        // Symplectic 2D periodic damped wave update (zero Dirichlet border bounce!)
        let t_sheet = clamp(self.sheet_tension, 0.04, 0.35);
        let k_restore = 0.0045f32;
        for _sub in 0..2 {
            for c in 0..cols_u {
                let cl = if c == 0 { cols_u - 1 } else { c - 1 };
                let cr = if c + 1 == cols_u { 0 } else { c + 1 };
                for r in 0..rows_u {
                    let ru = if r == 0 { rows_u - 1 } else { r - 1 };
                    let rd = if r + 1 == rows_u { 0 } else { r + 1 };
                    let idx = c * rows + r;
                    if idx >= grid_len { continue; }
                    let u_l = self.grid[cl * rows + r].u;
                    let u_r = self.grid[cr * rows + r].u;
                    let u_u = self.grid[c * rows + ru].u;
                    let u_d = self.grid[c * rows + rd].u;
                    let u_c = self.grid[idx].u;
                    let lap = u_l + u_r + u_u + u_d - 4.0 * u_c;
                    let damp_w = self.grid[idx].edge_damp;
                    let v_new = self.grid[idx].v + t_sheet * lap - k_restore * u_c - damp_w * self.grid[idx].v + self.grid[idx].f_load * 0.5;
                    self.grid[idx].v = clamp(v_new, -4.0, 4.0);
                }
            }
            for c in 0..cols_u {
                for r in 0..rows_u {
                    let idx = c * rows + r;
                    if idx < grid_len {
                        self.grid[idx].u = clamp(self.grid[idx].u + self.grid[idx].v, -12.0, 10.0);
                    }
                }
            }
        }

        for c in 0..cols_u {
            let cl = if c == 0 { cols_u - 1 } else { c - 1 };
            let cr = if c + 1 == cols_u { 0 } else { c + 1 };
            for r in 0..rows_u {
                let ru = if r == 0 { rows_u - 1 } else { r - 1 };
                let rd = if r + 1 == rows_u { 0 } else { r + 1 };
                let idx = c * rows + r;
                if idx >= grid_len { continue; }
                let sx = self.grid[cl * rows + r].u - self.grid[cr * rows + r].u;
                let sy = self.grid[c * rows + ru].u - self.grid[c * rows + rd].u;
                self.grid[idx].slope = clamp(sx * 0.65 + sy * 0.35, -3.5, 3.5);
            }
        }

        // Mirror periodic seam onto (cols-1, *) and (*, rows-1) for exact bilinear continuity
        for r in 0..rows_u {
            let src = self.grid[r];
            let dst = (cols - 1) * rows + r;
            if dst < grid_len {
                self.grid[dst].u = src.u;
                self.grid[dst].v = src.v;
                self.grid[dst].slope = src.slope;
            }
        }
        for c in 0..cols {
            let src_c = if c + 1 == cols { 0 } else { c };
            let src = self.grid[src_c * rows];
            let dst = c * rows + (rows - 1);
            if dst < grid_len {
                self.grid[dst].u = src.u;
                self.grid[dst].v = src.v;
                self.grid[dst].slope = src.slope;
            }
        }
    }

    fn step_em_field(&mut self, dt: f32) {
        let cols = self.grid_cols;
        let rows = self.grid_rows;
        let grid_len = (cols * rows).min(MAX_GRID_NODES);
        if cols <= 2 || rows <= 2 { return; }

        let dx_g = self.grid_dx.max(1.0);
        let dy_g = self.grid_dy.max(1.0);
        let cols_u = cols - 1;
        let rows_u = rows - 1;
        let count = self.particle_count.min(MAX_PARTICLES);

        for g in self.grid[..grid_len].iter_mut() {
            g.rho = 0.0;
            g.jx = 0.0;
            g.jy = 0.0;
        }

        for i in 0..count {
            let p = &self.particles[i];
            if p.charge == 0.0 { continue; }
            let gc = clamp(p.x / dx_g, 0.0, (cols_u - 1) as f32) as usize;
            let gr = clamp(p.y / dy_g, 0.0, (rows_u - 1) as f32) as usize;
            let idx = gc * rows + gr;
            if idx < grid_len {
                self.grid[idx].rho += p.charge;
                self.grid[idx].jx += p.charge * p.vx;
                self.grid[idx].jy += p.charge * p.vy;
            }
        }

        let w_span = self.width.max(1.0);
        let h_span = self.height.max(1.0);
        for k in self.knots.iter() {
            if !k.active { continue; }
            let ux = k.axis_angle.cos();
            let uy = k.axis_angle.sin();
            let nx = -uy;
            let ny = ux;
            let pole_off = k.cloak_radius * 0.72;

            if k.charge != 0.0 {
                let gc = clamp(k.x / dx_g, 0.0, (cols_u - 1) as f32) as usize;
                let gr = clamp(k.y / dy_g, 0.0, (rows_u - 1) as f32) as usize;
                let idx = gc * rows + gr;
                if idx < grid_len {
                    self.grid[idx].rho += k.charge * 16.0;
                    self.grid[idx].jx += k.charge * 16.0 * k.vx;
                    self.grid[idx].jy += k.charge * 16.0 * k.vy;
                }
            }

            let spin_mag = k.angular_vel.abs();
            if spin_mag > 0.05 && k.folding_progress > 0.2 {
                let dip_strength = (spin_mag * 3.2 + k.charge.abs() * 7.0) * k.folding_progress;
                let spin_sign = if k.angular_vel >= 0.0 { 1.0 } else { -1.0 };

                // North & South polar sources from spinning throat (tiled across borders)
                for &(p_dir, q_s) in &[(1.0f32, 1.0f32), (-1.0f32, -1.0f32)] {
                    let px = (k.x + ux * pole_off * p_dir).rem_euclid(w_span);
                    let py = (k.y + uy * pole_off * p_dir).rem_euclid(h_span);
                    let gc = clamp(px / dx_g, 0.0, (cols_u - 1) as f32) as usize;
                    let gr = clamp(py / dy_g, 0.0, (rows_u - 1) as f32) as usize;
                    let idx = gc * rows + gr;
                    if idx < grid_len {
                        self.grid[idx].rho += dip_strength * spin_sign * q_s;
                        self.grid[idx].jx += ux * dip_strength * 0.5;
                        self.grid[idx].jy += uy * dip_strength * 0.5;
                    }
                }

                // Equatorial azimuthal current loop -> Bz flux lobes (tiled across borders)
                let eq_off = k.cloak_radius * 0.55;
                for &side in &[-1.0f32, 1.0f32] {
                    let epx = (k.x + nx * eq_off * side).rem_euclid(w_span);
                    let epy = (k.y + ny * eq_off * side).rem_euclid(h_span);
                    let egc = clamp(epx / dx_g, 0.0, (cols_u - 1) as f32) as usize;
                    let egr = clamp(epy / dy_g, 0.0, (rows_u - 1) as f32) as usize;
                    let eidx = egc * rows + egr;
                    if eidx < grid_len {
                        self.grid[eidx].jx += -ux * side * dip_strength * 0.42 * spin_sign;
                        self.grid[eidx].jy += -uy * side * dip_strength * 0.42 * spin_sign;
                    }
                }
            }
        }

        // U(1) gauge coupling scales cleanly with energy frequency (0 at frequency=0 unless charged knots/particles exist)
        let has_knots = self.knots.iter().any(|k| k.active);
        let gauge_drive = if self.frequency <= 0.0 && !has_knots {
            0.0
        } else {
            clamp(self.frequency.max(0.25) * 0.07, 0.0, 2.5) * self.em_coupling
        };
        let inv_2dx = 1.0 / (2.0 * dx_g);
        let inv_2dy = 1.0 / (2.0 * dy_g);

        for c in 0..cols_u {
            let cl = if c == 0 { cols_u - 1 } else { c - 1 };
            let cr = if c + 1 == cols_u { 0 } else { c + 1 };
            for r in 0..rows_u {
                let ru = if r == 0 { rows_u - 1 } else { r - 1 };
                let rd = if r + 1 == rows_u { 0 } else { r + 1 };
                let idx = c * rows + r;
                if idx >= grid_len { continue; }
                let idx_l = cl * rows + r;
                let idx_r = cr * rows + r;
                let idx_u = c * rows + ru;
                let idx_d = c * rows + rd;

                let new_phi = 0.25 * (
                    self.grid[idx_l].phi + self.grid[idx_r].phi
                    + self.grid[idx_u].phi + self.grid[idx_d].phi
                    + self.grid[idx].rho * 0.42
                );
                self.grid[idx].phi = new_phi * 0.95;

                let prev_ax = self.grid[idx].ax;
                let prev_ay = self.grid[idx].ay;
                let avg_ax = 0.25 * (self.grid[idx_l].ax + self.grid[idx_r].ax + self.grid[idx_u].ax + self.grid[idx_d].ax);
                let avg_ay = 0.25 * (self.grid[idx_l].ay + self.grid[idx_r].ay + self.grid[idx_u].ay + self.grid[idx_d].ay);

                let new_ax = (avg_ax + self.grid[idx].jx * 0.08 * gauge_drive) * 0.92;
                let new_ay = (avg_ay + self.grid[idx].jy * 0.08 * gauge_drive) * 0.92;
                self.grid[idx].ax = new_ax;
                self.grid[idx].ay = new_ay;

                let dax_dt = (new_ax - prev_ax) / dt.max(0.01);
                let day_dt = (new_ay - prev_ay) / dt.max(0.01);

                let ex = -(self.grid[idx_r].phi - self.grid[idx_l].phi) * inv_2dx * 14.0 * gauge_drive - dax_dt * 0.35;
                let ey = -(self.grid[idx_d].phi - self.grid[idx_u].phi) * inv_2dy * 14.0 * gauge_drive - day_dt * 0.35;

                let day_dx = (self.grid[idx_r].ay - self.grid[idx_l].ay) * inv_2dx;
                let dax_dy = (self.grid[idx_d].ax - self.grid[idx_u].ax) * inv_2dy;
                let bz = (day_dx - dax_dy) * 18.0;

                self.grid[idx].ex = (self.grid[idx].ex * 0.78 + ex * 0.22).clamp(-12.0, 12.0);
                self.grid[idx].ey = (self.grid[idx].ey * 0.78 + ey * 0.22).clamp(-12.0, 12.0);
                self.grid[idx].bz = (self.grid[idx].bz * 0.80 + bz * 0.20).clamp(-5.0, 5.0);
            }
        }
    }

    fn step_knot_nucleation(&mut self, g_sim: f32, v_c: f32, v_c2: f32) {
        if g_sim <= 0.001 || self.base_mass <= 0.0001 {
            return;
        }

        let count = self.particle_count.min(MAX_PARTICLES);
        let cols = self.grid_cols;
        let rows = self.grid_rows;
        let cols_u = cols.saturating_sub(1).max(1);
        let rows_u = rows.saturating_sub(1).max(1);
        let dx_g = self.grid_dx.max(1.0);
        let dy_g = self.grid_dy.max(1.0);
        let spacing = self.grid_spacing.max(1.0);
        let c2 = (self.c * self.c).max(0.01);

        let sec_size = self.sector_size.max(55.0);
        let s_cols = ((self.width / sec_size).ceil() as usize).max(1).min(24);
        let s_rows = ((self.height / sec_size).ceil() as usize).max(1).min(20);
        let total_secs = (s_cols * s_rows).min(512);
        let w_span = self.width.max(1.0);
        let h_span = self.height.max(1.0);
        let grid_len = (cols * rows).min(MAX_GRID_NODES);

        for s in 0..total_secs {
            self.cell_mass[s] = 0.0;
            self.cell_x[s] = 0.0;
            self.cell_y[s] = 0.0;
            self.cell_vx[s] = 0.0;
            self.cell_vy[s] = 0.0;
            self.cell_vort[s] = 0.0;
            self.cell_dent[s] = 0.0;
            self.cell_count[s] = 0;
        }
        if cols > 2 && rows > 2 {
            for g in self.grid[..grid_len].iter_mut() {
                g.mass_dens = 0.0;
            }
        }

        // Random per-tick toroidal sub-cell offset eliminates static square grid seams across ticks
        let ox_shift = self.rand() * sec_size;
        let oy_shift = self.rand() * sec_size;
        let mass_per_particle = (self.base_mass / count.max(1) as f32) * 120.0;

        for i in 0..count {
            let p = &self.particles[i];
            if p.trapped_in >= 0 { continue; }
            let v2 = p.vx * p.vx + p.vy * p.vy;
            let beta2 = (v2 / v_c2).min(0.97);
            let gamma = 1.0 / (1.0 - beta2).sqrt();
            let m_i = mass_per_particle * p.freq_mult * gamma;

            if cols > 2 && rows > 2 && grid_len > 0 {
                let fx = clamp(p.x / dx_g, 0.0, cols_u as f32 - 0.0001);
                let fy = clamp(p.y / dy_g, 0.0, rows_u as f32 - 0.0001);
                let gc = fx.floor() as usize;
                let gr = fy.floor() as usize;
                let c1 = if gc + 1 >= cols_u { 0 } else { gc + 1 };
                let r1 = if gr + 1 >= rows_u { 0 } else { gr + 1 };
                let tx = fx - gc as f32;
                let ty = fy - gr as f32;
                let i00 = gc * rows + gr;
                let i10 = c1 * rows + gr;
                let i01 = gc * rows + r1;
                let i11 = c1 * rows + r1;
                if i00 < grid_len && i10 < grid_len && i01 < grid_len && i11 < grid_len {
                    self.grid[i00].mass_dens += m_i * (1.0 - tx) * (1.0 - ty);
                    self.grid[i10].mass_dens += m_i * tx * (1.0 - ty);
                    self.grid[i01].mass_dens += m_i * (1.0 - tx) * ty;
                    self.grid[i11].mass_dens += m_i * tx * ty;
                }
            }

            let sx = (p.x + ox_shift).rem_euclid(w_span);
            let sy = (p.y + oy_shift).rem_euclid(h_span);
            let sc = clamp(sx / sec_size, 0.0, (s_cols - 1) as f32) as usize;
            let sr = clamp(sy / sec_size, 0.0, (s_rows - 1) as f32) as usize;
            let s_idx = sc * s_rows + sr;
            if s_idx < total_secs {
                self.cell_mass[s_idx] += m_i;
                self.cell_x[s_idx] += sx * m_i;
                self.cell_y[s_idx] += sy * m_i;
                self.cell_dent[s_idx] = self.cell_dent[s_idx].max(p.dent);
                self.cell_count[s_idx] += 1;
            }
        }

        // Collapse barrier: combine Planck Mass with actual spacetime dent concentration!
        let collapse_thresh = self.planck_mass * 0.85;
        let mut best_seed: Option<(f32, f32, f32)> = None;

        for s_idx in 0..total_secs {
            if self.cell_count[s_idx] < 10 { continue; }
            let m_tot = self.cell_mass[s_idx];
            let dent_peak = self.cell_dent[s_idx];
            let m_eff = m_tot * (1.0 + clamp(dent_peak * 0.40, 0.0, 1.8));
            if m_eff < collapse_thresh { continue; }

            let seed_x = (self.cell_x[s_idx] / m_tot - ox_shift).rem_euclid(w_span);
            let seed_y = (self.cell_y[s_idx] / m_tot - oy_shift).rem_euclid(h_span);

            let mut blocked = false;
            for k in self.knots.iter() {
                if !k.active { continue; }
                let dx = torus_delta(k.x - seed_x, w_span);
                let dy = torus_delta(k.y - seed_y, h_span);
                let min_sep = (sec_size * 1.05).max(k.cloak_radius * 3.4);
                if dx * dx + dy * dy < min_sep * min_sep {
                    blocked = true;
                    break;
                }
            }
            if !blocked {
                for sw in self.shockwaves.iter() {
                    if !sw.active { continue; }
                    let dx = torus_delta(sw.x - seed_x, w_span);
                    let dy = torus_delta(sw.y - seed_y, h_span);
                    let r_lim = sw.radius + 55.0;
                    if dx * dx + dy * dy < r_lim * r_lim {
                        blocked = true;
                        break;
                    }
                }
            }
            if !blocked {
                let score = m_eff + dent_peak * 2.0;
                if best_seed.is_none() || score > best_seed.unwrap().2 {
                    best_seed = Some((seed_x, seed_y, score));
                }
            }
        }

        // Refine peak inside a true circular disk (zero square box boundaries!) and nucleate
        if let Some((seed_x, seed_y, _)) = best_seed {
            let r_disk = sec_size * 0.56;
            let r_disk2 = r_disk * r_disk;
            let mut m_disk = 0.0f32;
            let mut dx_com = 0.0f32;
            let mut dy_com = 0.0f32;
            let mut vx_sum = 0.0f32;
            let mut vy_sum = 0.0f32;
            let mut dent_disk = 0.0f32;
            let mut n_disk = 0usize;

            for i in 0..count {
                let p = &self.particles[i];
                if p.trapped_in >= 0 { continue; }
                let dx = torus_delta(p.x - seed_x, w_span);
                let dy = torus_delta(p.y - seed_y, h_span);
                if dx * dx + dy * dy <= r_disk2 {
                    let v2 = p.vx * p.vx + p.vy * p.vy;
                    let gamma = 1.0 / (1.0 - (v2 / v_c2).min(0.97)).sqrt();
                    let m_i = mass_per_particle * p.freq_mult * gamma;
                    m_disk += m_i;
                    dx_com += dx * m_i;
                    dy_com += dy * m_i;
                    vx_sum += p.vx;
                    vy_sum += p.vy;
                    dent_disk = dent_disk.max(p.dent);
                    n_disk += 1;
                }
            }

            let m_eff_disk = m_disk * (1.0 + clamp(dent_disk * 0.40, 0.0, 1.8));
            if n_disk >= 10 && m_eff_disk >= collapse_thresh {
                let kx = (seed_x + dx_com / m_disk).rem_euclid(w_span);
                let ky = (seed_y + dy_com / m_disk).rem_euclid(h_span);
                let mut blocked = false;
                for k in self.knots.iter() {
                    if !k.active { continue; }
                    let dx = torus_delta(k.x - kx, w_span);
                    let dy = torus_delta(k.y - ky, h_span);
                    let min_sep = (sec_size * 1.05).max(k.cloak_radius * 3.4);
                    if dx * dx + dy * dy < min_sep * min_sep {
                        blocked = true;
                        break;
                    }
                }
                if !blocked {
                    if let Some(k_slot) = (0..MAX_KNOTS).find(|&idx| !self.knots[idx].active) {
                        let kvx = (vx_sum / n_disk as f32) * 0.25;
                        let kvy = (vy_sum / n_disk as f32) * 0.25;

                        let mut vort = 0.0f32;
                        let mut l_disk = 0.0f32;
                        let mut i_disk = 0.0f32;
                        let mut knot_thirds: i32 = 0;
                        let mut partner_idx: Option<usize> = None;
                        for p_idx in 0..count {
                            let p = &mut self.particles[p_idx];
                            if p.trapped_in >= 0 { continue; }
                            let dx = torus_delta(p.x - kx, w_span);
                            let dy = torus_delta(p.y - ky, h_span);
                            let d2 = dx * dx + dy * dy;
                            if d2 > r_disk2 { continue; }
                            let m_i = p.freq_mult * self.base_mass;
                            let rvx = p.vx - kvx;
                            let rvy = p.vy - kvy;
                            let ang_i = dx * rvy - dy * rvx;
                            vort += ang_i;
                            l_disk += m_i * ang_i;
                            i_disk += m_i * d2;

                            if p.twist_thirds != 0 && (knot_thirds + p.twist_thirds as i32).abs() <= 3 {
                                knot_thirds += p.twist_thirds as i32;
                                p.twist_thirds = 0;
                                p.charge = 0.0;
                            } else if partner_idx.is_none() && p.twist_thirds == 0 {
                                partner_idx = Some(p_idx);
                            }
                        }

                        let vort_thirds: i32 = if vort > 6.0 { 3 }
                            else if vort < -6.0 { -3 }
                            else if vort > 2.5 { 2 }
                            else if vort < -2.5 { -2 }
                            else { 0 };

                        if vort_thirds != 0 {
                            if let Some(pi) = partner_idx {
                                let target_knot = (knot_thirds + vort_thirds).clamp(-3, 3);
                                let delta = target_knot - knot_thirds;
                                if delta != 0 {
                                    knot_thirds = target_knot;
                                    self.particles[pi].twist_thirds = (-delta) as i8;
                                    self.particles[pi].charge = (-delta) as f32 / 3.0;
                                }
                            }
                        }

                        let twists = twists_from_thirds(knot_thirds);
                        let q_topo = braid_charge(twists);
                        let r_raw = (m_disk * c2) / self.spacetime_tension.max(0.1);
                        let rad = clamp(4.5 + r_raw.sqrt() * 1.8, 6.0, 18.0);
                        let axis = if kvx.abs() + kvy.abs() > 0.04 {
                            kvy.atan2(kvx) + PI * 0.5
                        } else {
                            PI * 0.5
                        };
                        let omega_cloud = l_disk / i_disk.max(24.0);
                        let seed_bias = if q_topo != 0.0 {
                            self.initial_spin * (if q_topo > 0.0 { 0.16 } else { -0.16 })
                        } else {
                            self.initial_spin * 0.14
                        };
                        let init_spin = clamp(omega_cloud * 2.4 + seed_bias, -12.0, 12.0);

                        let tilt_cb = clamp(vort.abs() / (vort.abs() + 2.5) + 0.18, 0.18, 0.72);
                        let g_val = knot_grip(m_disk, self.planck_mass);
                        self.spin_acc[k_slot] = 0.0;
                        self.knots[k_slot] = Knot {
                            id: self.next_knot_id,
                            x: kx, y: ky, vx: kvx, vy: kvy,
                            mass: m_disk,
                            radius: rad,
                            cloak_radius: rad * 1.45,
                            strand_twists: twists,
                            charge: q_topo,
                            angular_vel: init_spin,
                            axis_angle: axis,
                            braid_phase: 0.0,
                            spew_acc: 0.0,
                            folding_progress: 0.08,
                            is_forming: true,
                            active: true,
                            is_quark: false,
                            color_vx: 0.0,
                            color_vy: 0.0,
                            trapped_cap_bonus: 0,
                            e_core: m_disk * c2,
                            e_trapped: 0.0,
                            axis_tilt_cb: tilt_cb,
                            spin_prec_w: 0.0,
                            grip: g_val,
                        };
                        self.next_knot_id += 1;

                        // Pressure well deepens spacetime: gently pinch inner disk radially inward so infalling
                        // particles carry their natural angular momentum into a spin-up top (no forced snap)
                        for p_idx in 0..count {
                            let p = &mut self.particles[p_idx];
                            if p.trapped_in >= 0 { continue; }
                            let dx = torus_delta(p.x - kx, w_span);
                            let dy = torus_delta(p.y - ky, h_span);
                            let d2 = dx * dx + dy * dy;
                            if d2 >= r_disk2 || d2 < 0.25 { continue; }
                            let dist = d2.sqrt();
                            let taper = 1.0 - d2 / r_disk2;
                            let pinch_in = (((g_sim * m_disk) / (dist + 14.0)).sqrt() * 0.09 * taper).min(v_c * 0.18);
                            p.vx -= (dx / dist) * pinch_in;
                            p.vy -= (dy / dist) * pinch_in;
                        }
                    }
                }
            }
        }

        // Update existing knots using isotropic circular disk mass from fine grid
        let r_keep = sec_size * 1.25;
        let r_keep2 = r_keep * r_keep;
        let ri_keep = (r_keep / spacing).ceil() as i32;
        for k_idx in 0..MAX_KNOTS {
            if !self.knots[k_idx].active { continue; }
            let kx = self.knots[k_idx].x;
            let ky = self.knots[k_idx].y;

            let mut local_m = 0.0f32;
            if cols > 2 && rows > 2 && grid_len > 0 {
                let gc_c = (kx / dx_g).round() as i32;
                let gr_c = (ky / dy_g).round() as i32;
                for dc in -ri_keep..=ri_keep {
                    let cc = (gc_c + dc).rem_euclid(cols_u as i32) as usize;
                    for dr in -ri_keep..=ri_keep {
                        let cr = (gr_c + dr).rem_euclid(rows_u as i32) as usize;
                        let gidx = cc * rows + cr;
                        if gidx < grid_len {
                            let dx = torus_delta(self.grid[gidx].ox - kx, w_span);
                            let dy = torus_delta(self.grid[gidx].oy - ky, h_span);
                            if dx * dx + dy * dy <= r_keep2 {
                                local_m += self.grid[gidx].mass_dens;
                            }
                        }
                    }
                }
            }
            let local_u_dent = if cols > 2 && rows > 2 {
                let gc = clamp(kx / dx_g, 0.0, (cols_u - 1) as f32) as usize;
                let gr = clamp(ky / dy_g, 0.0, (rows_u - 1) as f32) as usize;
                let idx = (gc * rows + gr).min(MAX_GRID_NODES - 1);
                (-self.grid[idx].u).max(0.0)
            } else {
                0.0
            };

            let k = &mut self.knots[k_idx];
            if local_m * (1.0 + local_u_dent * 0.35) >= collapse_thresh * 0.85 || k.is_quark {
                if k.folding_progress < 1.0 {
                    k.folding_progress = (k.folding_progress + 0.018 * self.sim_speed).min(1.0);
                    if k.folding_progress >= 1.0 {
                        k.is_forming = false;
                    }
                }
            } else if !(self.permanent_clumps && !k.is_forming && k.folding_progress >= 1.0) {
                k.folding_progress -= 0.02 * self.sim_speed;
                if k.folding_progress <= 0.0 {
                    let rem_thirds = k.strand_twists[0] as i32
                        + k.strand_twists[1] as i32
                        + k.strand_twists[2] as i32;
                    k.active = false;
                    for p_idx in 0..count {
                        if self.particles[p_idx].trapped_in == k_idx as i32 {
                            self.particles[p_idx].trapped_in = -1;
                        }
                    }
                    self.return_charge_to_field(rem_thirds);
                } else {
                    k.is_forming = true;
                }
            }
        }
    }

    fn step_knots_dynamics(&mut self, g_sim: f32, v_c: f32) {
        let count = self.particle_count.min(MAX_PARTICLES);
        let cols = self.grid_cols;
        let rows = self.grid_rows;
        let c2 = (self.c * self.c).max(0.01);
        let cx = self.width * 0.5;
        let cy = self.height * 0.5;

        let mut trapped_counts = [0i32; MAX_KNOTS];
        for i in 0..count {
            let tid = self.particles[i].trapped_in;
            if tid >= 0 && (tid as usize) < MAX_KNOTS {
                trapped_counts[tid as usize] += 1;
            }
        }

        // Update knot radii, grip, continuous infall/contraction spin-up, phase & polar spewing
        for k_idx in 0..MAX_KNOTS {
            if !self.knots[k_idx].active {
                self.spin_acc[k_idx] = 0.0;
                continue;
            }
            let mp = self.planck_mass;
            let acc_l = self.spin_acc[k_idx];
            self.spin_acc[k_idx] = 0.0;
            let k = &mut self.knots[k_idx];
            k.e_core = k.mass * c2;
            k.e_trapped = trapped_counts[k_idx] as f32 * self.base_mass * c2;
            k.grip = knot_grip(k.mass, mp);

            let eff_inertia_spin = (k.mass * (0.4 + 0.6 * k.grip)).max(1.0);
            if acc_l != 0.0 {
                k.angular_vel = clamp(k.angular_vel + acc_l / eff_inertia_spin, -8.0, 8.0);
            }
            let fp = clamp(k.folding_progress, 0.0, 1.0);
            let spin_sign = if k.angular_vel.abs() > 0.08 {
                if k.angular_vel >= 0.0 { 1.0 } else { -1.0 }
            } else if self.initial_spin >= 0.0 {
                1.0
            } else {
                -1.0
            };
            let min_top_spin = self.initial_spin.abs() * (0.12 + 0.88 * fp * fp);
            let target_w = spin_sign * k.angular_vel.abs().max(min_top_spin);
            k.angular_vel = clamp(lerp(k.angular_vel, target_w, (0.045 * self.sim_speed).min(0.45)), -8.0, 8.0);
            let rot_damp = (1.0 - 0.008 * self.sim_speed).max(0.85);
            if k.angular_vel.abs() > min_top_spin {
                k.angular_vel = spin_sign * min_top_spin.max(k.angular_vel.abs() * rot_damp);
            }
            let gyro_stab = clamp(k.angular_vel.abs() / (k.angular_vel.abs() + 1.5), 0.25, 1.0) * (0.35 + 0.65 * fp);
            k.axis_tilt_cb = lerp(k.axis_tilt_cb, gyro_stab, (0.04 * self.sim_speed).min(0.35));

            let r_eff = (k.e_core + k.e_trapped) / self.spacetime_tension.max(0.1);
            k.radius = clamp(4.0 + r_eff.sqrt() * 1.9, if k.is_quark { 4.5 } else { 6.0 }, if k.is_quark { 9.5 } else { 20.0 });
            k.cloak_radius = if k.is_quark { k.radius * 1.25 } else { k.radius * 1.45 };
            k.braid_phase += (k.angular_vel * 0.12 + self.frequency.max(0.0) * 0.025) * self.sim_speed;
            k.spew_acc += (k.angular_vel.abs() * 0.012 + trapped_counts[k_idx] as f32 * 0.0025)
                * self.frequency.max(0.0).sqrt() * self.sim_speed;
        }

        let w_span = self.width.max(1.0);
        let h_span = self.height.max(1.0);

        // Spin-driven polar proton ejection (+1 out polar gate, -1 sheath inside, conserving netQ!)
        for k_idx in 0..MAX_KNOTS {
            if !self.knots[k_idx].active || self.knots[k_idx].is_quark || self.knots[k_idx].folding_progress < 0.85 {
                continue;
            }
            if self.knots[k_idx].spew_acc >= 2.4 && self.frequency > 0.5 {
                self.knots[k_idx].spew_acc = 0.0;
                let k = self.knots[k_idx];
                let pole_sign = if (self.next_knot_id + k_idx as u32) % 2 == 0 { 1.0 } else { -1.0 };
                let ux = k.axis_angle.cos() * pole_sign;
                let uy = k.axis_angle.sin() * pole_sign;

                let mut proton_slot: Option<usize> = None;
                let mut sheath_slot: Option<usize> = None;
                for i in 0..count {
                    if self.particles[i].trapped_in == k_idx as i32 && self.particles[i].twist_thirds == 0 {
                        if proton_slot.is_none() { proton_slot = Some(i); }
                        else if sheath_slot.is_none() { sheath_slot = Some(i); break; }
                    }
                }
                if let (Some(pi), Some(si)) = (proton_slot, sheath_slot) {
                    let jet_speed = clamp(1.3 + k.angular_vel.abs() * 0.35, 1.1, v_c * 0.55);
                    let oblate_pole = 1.0 - clamp(k.angular_vel.abs() * 0.045, 0.0, 0.22);
                    let r_pole = k.cloak_radius * clamp(oblate_pole * 0.92, 0.62, 1.05);
                    let raw_px = k.x + ux * (r_pole + 0.6);
                    let raw_py = k.y + uy * (r_pole + 0.6);
                    let wx = raw_px.rem_euclid(w_span);
                    let wy = raw_py.rem_euclid(h_span);
                    self.particles[pi].trapped_in = -1;
                    self.particles[pi].twist_thirds = 3;
                    self.particles[pi].charge = 1.0;
                    self.particles[pi].x = wx;
                    self.particles[pi].y = wy;
                    if (wx - raw_px).abs() > 0.5 || (wy - raw_py).abs() > 0.5 {
                        self.particles[pi].prev_x = wx;
                        self.particles[pi].prev_y = wy;
                    } else {
                        self.particles[pi].prev_x = wx - ux * jet_speed * self.sim_speed;
                        self.particles[pi].prev_y = wy - uy * jet_speed * self.sim_speed;
                    }
                    self.particles[pi].vx = k.vx + ux * jet_speed;
                    self.particles[pi].vy = k.vy + uy * jet_speed;
                    self.particles[pi].polar_jet_age = 1.0;

                    self.particles[si].twist_thirds = -3;
                    self.particles[si].charge = -1.0;
                }
            }
        }

        // Pairwise knot interactions: flux bonds, Gravity, Dipole EM + Torque, 3D Vortex Drag, Tidal Inspiral & Smooth Black-Hole Merger
        let mut k_fx = [0.0f32; MAX_KNOTS];
        let mut k_fy = [0.0f32; MAX_KNOTS];
        let mut k_dw = [0.0f32; MAX_KNOTS];
        let mut shift_x_acc = [0.0f32; MAX_KNOTS];
        let mut shift_y_acc = [0.0f32; MAX_KNOTS];
        let mut knot_remap = [0i32; MAX_KNOTS];
        for idx in 0..MAX_KNOTS {
            knot_remap[idx] = idx as i32;
            self.tidal_dx[idx] = 0.0;
            self.tidal_dy[idx] = 0.0;
            self.tidal_pull[idx] = 0.0;
        }
        self.chirp_count = 0;

        let string_tension = 0.45 * self.gravity.max(1.0);
        for b_idx in 0..MAX_BONDS {
            if !self.bonds[b_idx].active { continue; }
            let a = self.bonds[b_idx].a;
            let b = self.bonds[b_idx].b;
            if !self.knots[a].active || !self.knots[b].active {
                self.bonds[b_idx].active = false;
                continue;
            }
            let dx = torus_delta(self.knots[b].x - self.knots[a].x, w_span);
            let dy = torus_delta(self.knots[b].y - self.knots[a].y, h_span);
            let dist = (dx * dx + dy * dy).sqrt().max(1.0);
            let c_dot = self.knots[a].color_vx * self.knots[b].color_vx + self.knots[a].color_vy * self.knots[b].color_vy;
            let f_conf = if c_dot < 0.0 { string_tension * (-c_dot * 2.0) } else { -string_tension * 0.5 };
            self.bonds[b_idx].tension = clamp(dist / 120.0, 0.25, 1.0);
            if dist > 24.0 {
                let fx = (dx / dist) * f_conf * self.knots[a].mass.min(self.knots[b].mass);
                let fy = (dy / dist) * f_conf * self.knots[a].mass.min(self.knots[b].mass);
                k_fx[a] += fx; k_fy[a] += fy;
                k_fx[b] -= fx; k_fy[b] -= fy;
            }
            if dist > 360.0 {
                self.bonds[b_idx].active = false;
                let mx = (self.knots[a].x + dx * 0.5).rem_euclid(w_span);
                let my = (self.knots[a].y + dy * 0.5).rem_euclid(h_span);
                self.spawn_shockwave(mx, my, 10.0, 85.0, 4.0, 1.5, 0.25, 3.0);
            }
        }

        let mut k_mu = [0.0f32; MAX_KNOTS];
        let mut k_mx = [1.0f32; MAX_KNOTS];
        let mut k_my = [0.0f32; MAX_KNOTS];
        if self.em_coupling > 0.0 {
            for idx in 0..MAX_KNOTS {
                if self.knots[idx].active {
                    k_mu[idx] = self.knots[idx].mass.max(0.1).powf(0.75) * self.knots[idx].folding_progress;
                    k_mx[idx] = self.knots[idx].axis_angle.cos();
                    k_my[idx] = self.knots[idx].axis_angle.sin();
                }
            }
        }

        let vortex_scale = (self.gravity_range * 2.5).max(80.0);
        for i in 0..MAX_KNOTS {
            if !self.knots[i].active { continue; }
            for j in (i + 1)..MAX_KNOTS {
                if !self.knots[j].active { continue; }
                let dx = torus_delta(self.knots[j].x - self.knots[i].x, w_span);
                let dy = torus_delta(self.knots[j].y - self.knots[i].y, h_span);
                let dist = (dx * dx + dy * dy).sqrt().max(1.0);
                let min_d = self.knots[i].cloak_radius + self.knots[j].cloak_radius;

                let m1 = self.knots[i].mass;
                let m2 = self.knots[j].mass;
                let g1 = self.knots[i].grip;
                let g2 = self.knots[j].grip;
                let eff_m1 = if self.individual_pull { m1 } else { self.planck_mass };
                let eff_m2 = if self.individual_pull { m2 } else { self.planck_mass };

                // 1. Grip-scaled Gravity + Coulomb EM
                let f_grav = (g_sim * 0.07 * eff_m1 * eff_m2 * g1 * g2) / (dist * dist + min_d * min_d);
                let f_em = -(self.knots[i].charge * self.knots[j].charge * 180.0 * self.em_coupling)
                    / (dist * dist + min_d * min_d);
                let tot_f = f_grav + f_em;
                let ux = dx / dist;
                let uy = dy / dist;
                k_fx[i] += ux * tot_f;
                k_fy[i] += uy * tot_f;
                k_fx[j] -= ux * tot_f;
                k_fy[j] -= uy * tot_f;

                // 2. 3D Out-of-Plane Spin Vortex Drag between folds (tilted axes twist neighbors into orbit)
                let d_soft = dist + 12.0;
                let x_att = d_soft / vortex_scale;
                let atten = 1.0 / (1.0 + x_att + 0.5 * x_att * x_att);
                let drag_i_on_j = (self.knots[i].angular_vel * m1 * (0.3 + 0.7 * self.knots[i].axis_tilt_cb) * 0.14 / (d_soft * d_soft.sqrt()) * atten)
                    .clamp(-0.35 * m2, 0.35 * m2);
                let drag_j_on_i = (self.knots[j].angular_vel * m2 * (0.3 + 0.7 * self.knots[j].axis_tilt_cb) * 0.14 / (d_soft * d_soft.sqrt()) * atten)
                    .clamp(-0.35 * m1, 0.35 * m1);
                k_fx[j] += -uy * drag_i_on_j;
                k_fy[j] += ux * drag_i_on_j;
                k_fx[i] += uy * drag_j_on_i;
                k_fy[i] += -ux * drag_j_on_i;

                // 3. Magnetic Dipole-Dipole Force & Alignment Torque
                if self.em_coupling > 0.0 {
                    let mu1 = k_mu[i];
                    let mu2 = k_mu[j];
                    let m1x = k_mx[i];
                    let m1y = k_my[i];
                    let m2x = k_mx[j];
                    let m2y = k_my[j];

                    let d_em = dist + 14.0;
                    let d3 = d_em * d_em * d_em;
                    let d4 = d3 * d_em;
                    let md1 = m1x * ux + m1y * uy;
                    let md2 = m2x * ux + m2y * uy;
                    let dd = m1x * m2x + m1y * m2y;

                    let f_dip = (self.em_coupling * 1400.0 * mu1 * mu2) / d4;
                    let mut fx_dip = f_dip * (md1 * m2x + md2 * m1x + (dd - 5.0 * md1 * md2) * ux);
                    let mut fy_dip = f_dip * (md1 * m2y + md2 * m1y + (dd - 5.0 * md1 * md2) * uy);
                    let dip_mag = (fx_dip * fx_dip + fy_dip * fy_dip).sqrt();
                    let dip_cap = 0.55 * m1.min(m2);
                    if dip_mag > dip_cap && dip_mag > 1e-6 {
                        fx_dip *= dip_cap / dip_mag;
                        fy_dip *= dip_cap / dip_mag;
                    }
                    k_fx[j] += fx_dip;
                    k_fy[j] += fy_dip;
                    k_fx[i] -= fx_dip;
                    k_fy[i] -= fy_dip;

                    let bx_1on2 = (3.0 * md1 * ux - m1x) * mu1 / d3;
                    let by_1on2 = (3.0 * md1 * uy - m1y) * mu1 / d3;
                    let bx_2on1 = (3.0 * md2 * ux - m2x) * mu2 / d3;
                    let by_2on1 = (3.0 * md2 * uy - m2y) * mu2 / d3;

                    let k_torq = self.em_coupling * 95.0;
                    k_dw[j] += k_torq * mu2 * (m2x * by_1on2 - m2y * bx_1on2)
                        / (1.0 + self.knots[j].angular_vel.abs() + m2);
                    k_dw[i] += k_torq * mu1 * (m1x * by_2on1 - m1y * bx_2on1)
                        / (1.0 + self.knots[i].angular_vel.abs() + m1);
                }

                // 4. Soliton Repulsion (ON) OR Black-Hole Tidal Inspiral & Ripple Coalescence (OFF)
                if self.non_destructible || (self.knots[i].is_quark && self.knots[j].is_quark) {
                    let repel_r = min_d * 2.1;
                    if dist < repel_r && dist >= min_d * 0.85 {
                        let tf = (repel_r - dist) / repel_r;
                        let f_rep = (g_sim * 0.40 * eff_m1 * eff_m2 * tf * tf) / (dist * dist + min_d * min_d);
                        k_fx[i] -= ux * f_rep;
                        k_fy[i] -= uy * f_rep;
                        k_fx[j] += ux * f_rep;
                        k_fy[j] += uy * f_rep;
                    }
                } else if dist < min_d * 2.6 && dist >= min_d * 0.90 {
                    // Mutual tidal stretching, gravitational-wave inspiral damping & quadrupolar chirp ripples
                    let tidal = ((min_d * 2.6 - dist) / (min_d * 1.7)).clamp(0.0, 1.0);
                    let tidal2 = tidal * tidal;
                    if tidal > self.tidal_pull[i] {
                        self.tidal_pull[i] = tidal;
                        self.tidal_dx[i] = ux;
                        self.tidal_dy[i] = uy;
                    }
                    if tidal > self.tidal_pull[j] {
                        self.tidal_pull[j] = tidal;
                        self.tidal_dx[j] = -ux;
                        self.tidal_dy[j] = -uy;
                    }
                    // Tidal attraction + GW orbital energy loss draws binary smoothly into coalescence
                    let tm = m1 + m2;
                    let damp_gw = 0.036 * tidal2 * (m1.min(m2) / tm.max(1.0));
                    let rvx = self.knots[j].vx - self.knots[i].vx;
                    let rvy = self.knots[j].vy - self.knots[i].vy;
                    k_fx[i] += rvx * m1 * damp_gw + ux * tidal2 * g_sim * 0.04 * m1.min(m2);
                    k_fy[i] += rvy * m1 * damp_gw + uy * tidal2 * g_sim * 0.04 * m1.min(m2);
                    k_fx[j] -= rvx * m2 * damp_gw + ux * tidal2 * g_sim * 0.04 * m1.min(m2);
                    k_fy[j] -= rvy * m2 * damp_gw + uy * tidal2 * g_sim * 0.04 * m1.min(m2);

                    if tidal > 0.16 && self.chirp_count < 32 {
                        let mid_x = (self.knots[i].x + dx * 0.5).rem_euclid(w_span);
                        let mid_y = (self.knots[i].y + dy * 0.5).rem_euclid(h_span);
                        let ang = dy.atan2(dx) + self.time * 0.45;
                        self.chirp_pulses[self.chirp_count] = (mid_x, mid_y, 0.24 * tidal2, ang);
                        self.chirp_count += 1;
                    }
                }

                let contact_d = if !self.non_destructible && !(self.knots[i].is_quark && self.knots[j].is_quark) {
                    min_d * 0.90
                } else {
                    min_d * 0.85
                };

                if dist < contact_d {
                    let vn = (self.knots[j].vx - self.knots[i].vx) * ux
                        + (self.knots[j].vy - self.knots[i].vy) * uy;

                    if !self.non_destructible && !(self.knots[i].is_quark && self.knots[j].is_quark) {
                        // Black-Hole Smooth Coalescence: heavier absorbs lighter, radiates pure metric ripples (ZERO kinetic blast!)
                        let (survivor, absorbed, dx_sa, dy_sa, ms, ma) = if m1 >= m2 {
                            (i, j, dx, dy, m1, m2)
                        } else {
                            (j, i, -dx, -dy, m2, m1)
                        };
                        let tm = ms + ma;
                        let shift_com_x = dx_sa * (ma / tm);
                        let shift_com_y = dy_sa * (ma / tm);
                        let com_x = (self.knots[survivor].x + shift_com_x).rem_euclid(w_span);
                        let com_y = (self.knots[survivor].y + shift_com_y).rem_euclid(h_span);
                        let vx_new = (ms * self.knots[survivor].vx + ma * self.knots[absorbed].vx) / tm;
                        let vy_new = (ms * self.knots[survivor].vy + ma * self.knots[absorbed].vy) / tm;

                        let sum_thirds = (self.knots[survivor].strand_twists[0] as i32
                            + self.knots[survivor].strand_twists[1] as i32
                            + self.knots[survivor].strand_twists[2] as i32)
                            + (self.knots[absorbed].strand_twists[0] as i32
                            + self.knots[absorbed].strand_twists[1] as i32
                            + self.knots[absorbed].strand_twists[2] as i32);
                        let clamped = sum_thirds.clamp(-3, 3);
                        let rem = sum_thirds - clamped;
                        let orb_l = dx_sa * (self.knots[absorbed].vy - self.knots[survivor].vy)
                            - dy_sa * (self.knots[absorbed].vx - self.knots[survivor].vx);

                        self.knots[survivor].x = com_x;
                        self.knots[survivor].y = com_y;
                        self.knots[survivor].vx = vx_new;
                        self.knots[survivor].vy = vy_new;
                        self.knots[survivor].mass = tm;
                        self.knots[survivor].grip = knot_grip(tm, self.planck_mass);
                        self.knots[survivor].angular_vel = clamp(
                            (ms * self.knots[survivor].angular_vel + ma * self.knots[absorbed].angular_vel) / tm
                                + orb_l * 0.025,
                            -12.0,
                            12.0,
                        );
                        self.knots[survivor].axis_tilt_cb = clamp(
                            (self.knots[survivor].axis_tilt_cb * ms + self.knots[absorbed].axis_tilt_cb * ma) / tm
                                + (orb_l.abs() / (orb_l.abs() + 20.0)) * 0.15,
                            0.20,
                            1.0,
                        );
                        self.knots[survivor].strand_twists = twists_from_thirds(clamped);
                        self.knots[survivor].charge = braid_charge(self.knots[survivor].strand_twists);
                        self.knots[survivor].trapped_cap_bonus += self.knots[absorbed].trapped_cap_bonus + 8;
                        self.knots[survivor].folding_progress = self.knots[survivor]
                            .folding_progress
                            .max(self.knots[absorbed].folding_progress)
                            .max(0.5);
                        if self.knots[survivor].folding_progress >= 1.0 {
                            self.knots[survivor].is_forming = false;
                        }
                        self.knots[absorbed].active = false;
                        knot_remap[absorbed] = survivor as i32;
                        shift_x_acc[survivor] += shift_com_x;
                        shift_y_acc[survivor] += shift_com_y;
                        self.tidal_pull[survivor] = self.tidal_pull[survivor].max(0.55);

                        // Pure gravitational ringdown ripples (kinetic_push = 0.0 so ZERO kinetic particle explosion!)
                        let ripple_max_r = clamp(120.0 + tm * 1.4, 130.0, 240.0);
                        let ripple_amp = clamp(0.32 + tm * 0.008, 0.32, 0.65);
                        self.spawn_shockwave(
                            com_x,
                            com_y,
                            min_d * 0.38,
                            ripple_max_r,
                            tm * 0.25,
                            0.0,
                            ripple_amp,
                            2.5,
                        );
                        if self.chirp_count < 32 {
                            self.chirp_pulses[self.chirp_count] = (com_x, com_y, 0.55, dy.atan2(dx));
                            self.chirp_count += 1;
                        }
                        self.return_charge_to_field(rem);
                        if survivor == i {
                            continue;
                        } else {
                            break;
                        }
                    } else {
                        // Soliton Repulsion ON: elastic core bounce (no O(N) particle loop inside!)
                        let overlap = contact_d - dist;
                        let tm = m1 + m2;
                        let sx_i = -ux * overlap * (m2 / tm);
                        let sy_i = -uy * overlap * (m2 / tm);
                        let sx_j = ux * overlap * (m1 / tm);
                        let sy_j = uy * overlap * (m1 / tm);
                        self.knots[i].x = (self.knots[i].x + sx_i).rem_euclid(w_span);
                        self.knots[i].y = (self.knots[i].y + sy_i).rem_euclid(h_span);
                        self.knots[j].x = (self.knots[j].x + sx_j).rem_euclid(w_span);
                        self.knots[j].y = (self.knots[j].y + sy_j).rem_euclid(h_span);
                        shift_x_acc[i] += sx_i;
                        shift_y_acc[i] += sy_i;
                        shift_x_acc[j] += sx_j;
                        shift_y_acc[j] += sy_j;

                        if vn < 0.0 {
                            let imp = -(1.7 * vn) / (1.0 / m1 + 1.0 / m2);
                            self.knots[i].vx -= (imp / m1) * ux;
                            self.knots[i].vy -= (imp / m1) * uy;
                            self.knots[j].vx += (imp / m2) * ux;
                            self.knots[j].vy += (imp / m2) * uy;
                        }
                    }
                }
            }
        }

        // Flatten merger remap chains
        for k_idx in 0..MAX_KNOTS {
            let mut cur = knot_remap[k_idx];
            if cur >= 0 && (cur as usize) < MAX_KNOTS {
                let mut steps = 0;
                while cur >= 0 && (cur as usize) < MAX_KNOTS && knot_remap[cur as usize] != cur && steps < 8 {
                    cur = knot_remap[cur as usize];
                    steps += 1;
                }
                knot_remap[k_idx] = if cur >= 0 && (cur as usize) < MAX_KNOTS && self.knots[cur as usize].active {
                    cur
                } else {
                    -1
                };
            }
        }

        // Integrate active knots & accumulate net toroidal displacement
        for i in 0..MAX_KNOTS {
            if !self.knots[i].active { continue; }
            let eff_inertia = (self.knots[i].mass * (0.4 + 0.6 * self.knots[i].grip)).max(1.0);
            let mut ax = k_fx[i] / eff_inertia;
            let mut ay = k_fy[i] / eff_inertia;

            if self.knots[i].charge != 0.0 && cols > 2 && rows > 2 {
                let gc = clamp(self.knots[i].x / self.grid_dx.max(1.0), 0.0, (cols - 2) as f32) as usize;
                let gr = clamp(self.knots[i].y / self.grid_dy.max(1.0), 0.0, (rows - 2) as f32) as usize;
                let g = &self.grid[gc * rows + gr];
                ax += self.knots[i].charge * (g.ex + self.knots[i].vy * g.bz) * 0.08;
                ay += self.knots[i].charge * (g.ey - self.knots[i].vx * g.bz) * 0.08;
            }

            if self.center_bias > 0.0 {
                let dcx = cx - self.knots[i].x;
                let dcy = cy - self.knots[i].y;
                let dc = (dcx * dcx + dcy * dcy).sqrt().max(1.0);
                if dc > 35.0 {
                    ax += (dcx / dc) * 0.03 * self.center_bias;
                    ay += (dcy / dc) * 0.03 * self.center_bias;
                }
            }

            if cols > 2 && rows > 2 {
                let cols_u = cols - 1;
                let rows_u = rows - 1;
                let gc = (self.knots[i].x / self.grid_dx.max(1.0)).floor().clamp(0.0, (cols_u - 1) as f32) as usize;
                let gr = (self.knots[i].y / self.grid_dy.max(1.0)).floor().clamp(0.0, (rows_u - 1) as f32) as usize;
                let cl = if gc == 0 { cols_u - 1 } else { gc - 1 };
                let cr = if gc + 1 == cols_u { 0 } else { gc + 1 };
                let ru = if gr == 0 { rows_u - 1 } else { gr - 1 };
                let rd = if gr + 1 == rows_u { 0 } else { gr + 1 };
                let dux = (self.grid[cr * rows + gr].u - self.grid[cl * rows + gr].u) * 0.5;
                let duy = (self.grid[gc * rows + rd].u - self.grid[gc * rows + ru].u) * 0.5;
                let w_inv = 1.0 / (1.0 + self.knots[i].mass / 12.0);
                ax += -dux * 0.12 * w_inv;
                ay += -duy * 0.12 * w_inv;
            }

            let k = &mut self.knots[i];
            k.spin_prec_w = clamp((k.spin_prec_w + k_dw[i] * self.sim_speed) * (1.0 - 0.14 * self.sim_speed).max(0.25), -2.5, 2.5);
            k.axis_angle += (k.angular_vel * 0.008 + k.spin_prec_w * 0.06) * self.sim_speed;

            k.vx = (k.vx + ax * self.sim_speed * 0.55) * 0.986;
            k.vy = (k.vy + ay * self.sim_speed * 0.55) * 0.986;
            let k_spd = (k.vx * k.vx + k.vy * k.vy).sqrt();
            if k_spd > 1.6 {
                let cap = 1.6 / k_spd;
                k.vx *= cap;
                k.vy *= cap;
            }

            let step_sx = k.vx * self.sim_speed;
            let step_sy = k.vy * self.sim_speed;
            let mut new_x = k.x + step_sx;
            let mut new_y = k.y + step_sy;
            if !new_x.is_finite() { new_x = cx; }
            if !new_y.is_finite() { new_y = cy; }
            k.x = new_x.rem_euclid(w_span);
            k.y = new_y.rem_euclid(h_span);
            shift_x_acc[i] += step_sx;
            shift_y_acc[i] += step_sy;
        }

        // Single O(N) pass to remap merged knots cleanly (no teleport jumps for absorbed outer rings)
        for p in self.particles[..count].iter_mut() {
            let tid = p.trapped_in;
            if tid >= 0 && (tid as usize) < MAX_KNOTS {
                let target = knot_remap[tid as usize];
                if target != tid {
                    // Host knot merged into survivor: release outer orbital cloud cleanly in-place
                    if target >= 0 {
                        let tk = self.knots[target as usize];
                        let dx = torus_delta(p.x - tk.x, w_span);
                        let dy = torus_delta(p.y - tk.y, h_span);
                        if dx * dx + dy * dy <= (tk.cloak_radius * 0.85) * (tk.cloak_radius * 0.85) {
                            p.trapped_in = target;
                        } else {
                            p.trapped_in = -1;
                        }
                    } else {
                        p.trapped_in = -1;
                    }
                    continue;
                }
                p.trapped_in = target;
                if target >= 0 {
                    let ti = target as usize;
                    let sx = shift_x_acc[ti];
                    let sy = shift_y_acc[ti];
                    if sx != 0.0 || sy != 0.0 {
                        let px_raw = p.x + sx;
                        let py_raw = p.y + sy;
                        let wx = px_raw.rem_euclid(w_span);
                        let wy = py_raw.rem_euclid(h_span);
                        if (wx - px_raw).abs() > 0.5 || (wy - py_raw).abs() > 0.5 {
                            p.prev_x = wx;
                            p.prev_y = wy;
                        } else {
                            p.prev_x += sx;
                            p.prev_y += sy;
                        }
                        p.x = wx;
                        p.y = wy;
                    }
                }
            }
        }
    }

    /// O(N) Cloud-In-Cell particle number density + isotropic 9-point smoothing
    /// for Quantum Pauli Exclusion / Degeneracy Pressure (zero grid checkerboarding).
    fn step_pauli_degeneracy(&mut self) {
        let cols = self.grid_cols;
        let rows = self.grid_rows;
        let grid_len = (cols * rows).min(MAX_GRID_NODES);
        if cols <= 2 || rows <= 2 { return; }
        let cols_u = cols - 1;
        let rows_u = rows - 1;

        let dx_g = self.grid_dx.max(1.0);
        let dy_g = self.grid_dy.max(1.0);
        let count = self.particle_count.min(MAX_PARTICLES);
        for g in self.grid[..grid_len].iter_mut() {
            g.num_dens = 0.0;
        }

        for i in 0..count {
            let p = &self.particles[i];
            let fx = clamp(p.x / dx_g, 0.0, cols_u as f32 - 0.0001);
            let fy = clamp(p.y / dy_g, 0.0, rows_u as f32 - 0.0001);
            let gc = fx.floor() as usize;
            let gr = fy.floor() as usize;
            let c1 = if gc + 1 >= cols_u { 0 } else { gc + 1 };
            let r1 = if gr + 1 >= rows_u { 0 } else { gr + 1 };
            let tx = fx - gc as f32;
            let ty = fy - gr as f32;

            let i00 = gc * rows + gr;
            let i10 = c1 * rows + gr;
            let i01 = gc * rows + r1;
            let i11 = c1 * rows + r1;
            if i00 < grid_len && i10 < grid_len && i01 < grid_len && i11 < grid_len {
                self.grid[i00].num_dens += (1.0 - tx) * (1.0 - ty);
                self.grid[i10].num_dens += tx * (1.0 - ty);
                self.grid[i01].num_dens += (1.0 - tx) * ty;
                self.grid[i11].num_dens += tx * ty;
            }
        }

        // Single-pass periodic 9-point isotropic Gaussian filter [1,2,1; 2,4,2; 1,2,1]/16
        for idx in 0..grid_len {
            self.smooth_tmp[idx] = self.grid[idx].num_dens;
        }
        for c in 0..cols_u {
            let cl = if c == 0 { cols_u - 1 } else { c - 1 };
            let cr = if c + 1 == cols_u { 0 } else { c + 1 };
            for r in 0..rows_u {
                let ru = if r == 0 { rows_u - 1 } else { r - 1 };
                let rd = if r + 1 == rows_u { 0 } else { r + 1 };
                let idx = c * rows + r;
                if idx >= grid_len { continue; }
                let s = 4.0 * self.smooth_tmp[idx]
                    + 2.0 * (self.smooth_tmp[cl * rows + r] + self.smooth_tmp[cr * rows + r]
                           + self.smooth_tmp[c * rows + ru] + self.smooth_tmp[c * rows + rd])
                    + (self.smooth_tmp[cl * rows + ru] + self.smooth_tmp[cr * rows + ru]
                     + self.smooth_tmp[cl * rows + rd] + self.smooth_tmp[cr * rows + rd]);
                self.grid[idx].num_dens = s * 0.0625;
            }
        }
        for r in 0..rows_u {
            let dst = (cols - 1) * rows + r;
            if dst < grid_len {
                self.grid[dst].num_dens = self.grid[r].num_dens;
            }
        }
        for c in 0..cols {
            let src_c = if c + 1 == cols { 0 } else { c };
            let dst = c * rows + (rows - 1);
            if dst < grid_len {
                self.grid[dst].num_dens = self.grid[src_c * rows].num_dens;
            }
        }
    }

    fn build_knot_acceleration_fields(&mut self, g_sim: f32, v_c: f32, c2: f32) {
        let w_span = self.width.max(1.0);
        let h_span = self.height.max(1.0);
        let vortex_scale = (self.gravity_range * 1.8).max(70.0);

        self.ak_count = 0;
        let mut max_end_d = 0.0f32;
        for k_idx in 0..MAX_KNOTS {
            let k = self.knots[k_idx];
            if !k.active { continue; }
            let mass_scale = (k.mass / self.planck_mass.max(0.01)).sqrt();
            let eff_range = if self.individual_pull {
                self.gravity_range * clamp(0.6 + 0.4 * mass_scale, 0.5, 2.5)
            } else {
                self.gravity_range
            };
            let start_d = eff_range * 0.5;
            let end_d = eff_range * 1.5;
            if end_d > max_end_d {
                max_end_d = end_d;
            }
            let r = k.radius.max(5.5);
            let j_spin = 0.4 * k.mass * r * r * k.angular_vel;
            let spin_drive = (1.0 + self.orbital_boost * 0.008) * (0.3 + 0.7 * k.axis_tilt_cb);
            let twist_sum = (k.strand_twists[0].abs() + k.strand_twists[1].abs() + k.strand_twists[2].abs()) as f32;

            let fp = clamp(k.folding_progress, 0.0, 1.0);
            let fp_smooth = fp * fp * (3.0 - 2.0 * fp);
            self.ak_buf[self.ak_count] = ActiveKnot {
                idx: k_idx,
                x: k.x,
                y: k.y,
                vx: k.vx,
                vy: k.vy,
                mass: k.mass,
                radius: k.radius,
                cloak_radius: k.cloak_radius,
                angular_vel: k.angular_vel,
                axis_tilt_cb: k.axis_tilt_cb,
                braid_phase: k.braid_phase,
                bp_cos: fast_cos(k.braid_phase),
                bp_sin: fast_sin(k.braid_phase),
                tidal_pull: self.tidal_pull[k_idx],
                tidal_dx: self.tidal_dx[k_idx],
                tidal_dy: self.tidal_dy[k_idx],
                mx: k.axis_angle.cos(),
                my: k.axis_angle.sin(),
                ux: k.axis_angle.cos(),
                uy: k.axis_angle.sin(),
                mu_k: k.mass.max(0.1).powf(0.75) * fp,
                grip: k.grip,
                f_scale: fp_smooth.max(0.08),
                folding_progress: fp,
                is_quark: k.is_quark,
                j_spin_drive: 0.18 * g_sim * j_spin * spin_drive * fp_smooth,
                start_d,
                end_d,
                end_d2: end_d * end_d,
                braid_gain: (0.60 + twist_sum * 0.18) * fp,
                handedness: if k.charge < 0.0 { -1.0 } else { 1.0 },
            };
            self.ak_count += 1;
        }

        // Near-field periodic spatial hash covering up to KNOT_R_HI = 58.0 u
        let k_cols = (w_span / KNOT_R_HI).floor().clamp(3.0, 24.0) as usize;
        let k_rows = (h_span / KNOT_R_HI).floor().clamp(3.0, 20.0) as usize;
        self.k_cols = k_cols;
        self.k_rows = k_rows;
        self.k_cell_w = w_span / k_cols as f32;
        self.k_cell_h = h_span / k_rows as f32;
        let k_cells_tot = k_cols * k_rows;
        for h in self.k_cell_head[..k_cells_tot].iter_mut() {
            *h = -1;
        }
        for ai in 0..self.ak_count {
            let ak = self.ak_buf[ai];
            let kc = ((ak.x / self.k_cell_w).floor() as i32).rem_euclid(k_cols as i32) as usize;
            let kr = ((ak.y / self.k_cell_h).floor() as i32).rem_euclid(k_rows as i32) as usize;
            let cell = kc * k_rows + kr;
            self.k_cell_next[ai] = self.k_cell_head[cell];
            self.k_cell_head[cell] = ai as i32;
        }

        // Coarse periodic far-field mesh for gravity_range > KNOT_R_LO
        let has_forces = (g_sim > 0.0 || self.em_coupling > 0.0) && self.gravity_range > 0.0;
        self.use_far_field = self.ak_count > 0 && has_forces && max_end_d > KNOT_R_LO;
        if !self.use_far_field {
            return;
        }

        let fc = (w_span / 22.0).round().clamp(10.0, 48.0) as usize;
        let fr = (h_span / 22.0).round().clamp(8.0, 36.0) as usize;
        self.far_cols = fc;
        self.far_rows = fr;
        self.far_dx = w_span / fc as f32;
        self.far_dy = h_span / fr as f32;
        let f_tot = fc * fr;
        for n in self.far_grid[..f_tot].iter_mut() {
            *n = FarFieldNode::default();
        }

        let r_lo2 = KNOT_R_LO * KNOT_R_LO;
        let inv_blend = 1.0 / (KNOT_R_HI - KNOT_R_LO);
        let do_em = self.em_coupling > 0.0;
        let do_g = g_sim > 0.0;
        let inv_vortex_scale = 1.0 / vortex_scale;

        for ai in 0..self.ak_count {
            let ak = self.ak_buf[ai];
            if ak.end_d <= KNOT_R_LO { continue; }
            let kc_c = (ak.x / self.far_dx).round() as i32;
            let kr_c = (ak.y / self.far_dy).round() as i32;
            let use_bbox = ak.end_d < w_span * 0.45 && ak.end_d < h_span * 0.45;
            let rc = if use_bbox { (ak.end_d / self.far_dx).ceil() as i32 + 1 } else { 0 };
            let rr = if use_bbox { (ak.end_d / self.far_dy).ceil() as i32 + 1 } else { 0 };
            let c_start = if use_bbox { kc_c - rc } else { 0 };
            let c_end = if use_bbox { kc_c + rc } else { fc as i32 - 1 };
            let r_start = if use_bbox { kr_c - rr } else { 0 };
            let r_end = if use_bbox { kr_c + rr } else { fr as i32 - 1 };

            let inv_range_span = 1.0 / (ak.end_d - ak.start_d).max(1.0);
            let inv_cloak = 1.0 / ak.cloak_radius.max(1.0);

            for ci in c_start..=c_end {
                let c = ci.rem_euclid(fc as i32) as usize;
                let gx = c as f32 * self.far_dx;
                let dx = torus_delta(ak.x - gx, w_span);
                let dx2 = dx * dx;
                if dx2 >= ak.end_d2 { continue; }

                for ri in r_start..=r_end {
                    let r = ri.rem_euclid(fr as i32) as usize;
                    let gy = r as f32 * self.far_dy;
                    let dy = torus_delta(ak.y - gy, h_span);
                    let d2 = dx2 + dy * dy;
                    if d2 >= ak.end_d2 || d2 <= r_lo2 { continue; }

                    let dist = d2.sqrt().max(1.0);
                    let w_far = if dist >= KNOT_R_HI {
                        1.0
                    } else {
                        let s = (dist - KNOT_R_LO) * inv_blend;
                        s * s * (3.0 - 2.0 * s)
                    };
                    let rf = if dist <= ak.start_d {
                        1.0
                    } else {
                        let t = clamp((dist - ak.start_d) * inv_range_span, 0.0, 1.0);
                        1.0 - t * t * (3.0 - 2.0 * t)
                    };
                    let w_eff = rf * w_far;
                    if w_eff <= 0.0001 { continue; }

                    let inv_d = 1.0 / dist;
                    let ux_kp = dx * inv_d;
                    let uy_kp = dy * inv_d;
                    let node = &mut self.far_grid[c * fr + r];

                    if do_g {
                        let f_g = ((g_sim * ak.mass * ak.grip * 0.075) / (d2 + 36.0) * w_eff).min(0.45) * ak.f_scale;
                        node.gx += ux_kp * f_g;
                        node.gy += uy_kp * f_g;

                        let spin_blend = 0.06 * w_eff * (0.15 + 0.85 * ak.folding_progress);
                        if spin_blend > 0.00001 {
                            let x_att = dist * inv_vortex_scale;
                            let atten = 1.0 / (1.0 + x_att + 0.5 * x_att * x_att);
                            let v_drag = ((ak.j_spin_drive * atten) / (c2 * (d2 + 28.0)))
                                .clamp(-v_c * 0.48, v_c * 0.48);
                            let tx = uy_kp;
                            let ty = -ux_kp;
                            node.gx += (tx * v_drag + ak.vx) * spin_blend;
                            node.gy += (ty * v_drag + ak.vy) * spin_blend;
                            node.vdamp += spin_blend;
                        }
                    }

                    if do_em {
                        let r_soft = dist + 14.0;
                        let inv_rs = 1.0 / r_soft;
                        let rx_u = -dx * inv_rs;
                        let ry_u = -dy * inv_rs;
                        let md = ak.mx * rx_u + ak.my * ry_u;
                        let d3_inv = inv_rs * inv_rs * inv_rs;
                        let scale_b = ak.mu_k * d3_inv * w_far;
                        node.bx += (3.0 * md * rx_u - ak.mx) * scale_b;
                        node.by += (3.0 * md * ry_u - ak.my) * scale_b;
                    }

                    let cl = clamp(1.0 - (dist * inv_cloak - 1.0) * 0.25, 0.0, 1.0) * w_far;
                    if cl > node.closeness {
                        node.closeness = cl;
                    }
                }
            }
        }
    }

    fn step_particles(&mut self, g_sim: f32, v_c: f32, v_c2: f32) {
        self.build_knot_acceleration_fields(g_sim, v_c, v_c2);

        let count = self.particle_count.min(MAX_PARTICLES);
        let cols = self.grid_cols;
        let rows = self.grid_rows;
        let dx_g = self.grid_dx.max(1.0);
        let dy_g = self.grid_dy.max(1.0);
        let c2 = (self.c * self.c).max(0.01);
        let cx = self.width * 0.5;
        let cy = self.height * 0.5;
        let damp = (1.0 - clamp(self.drag, 0.0, 0.98)).powf(self.sim_speed);
        let freq_pos = self.frequency.max(0.0);
        let t_wave = self.time * freq_pos * 0.08;
        let psi_rad = clamp(self.pole_gate_deg, 5.0, 90.0) * (PI / 180.0);
        let sin_psi = psi_rad.sin();

        let mut trapped_counts = [0i32; MAX_KNOTS];
        for i in 0..count {
            let tid = self.particles[i].trapped_in;
            if tid >= 0 && (tid as usize) < MAX_KNOTS {
                trapped_counts[tid as usize] += 1;
            }
        }

        let w_span = self.width.max(1.0);
        let h_span = self.height.max(1.0);
        let grid_len = (cols * rows).min(MAX_GRID_NODES);
        let n_avg = (count as f32 * dx_g * dy_g) / (w_span * h_span);
        let n_pauli = (n_avg * 2.0).max(2.6);
        let vortex_scale = (self.gravity_range * 1.8).max(70.0);
        let k_cols = self.k_cols.max(1);
        let k_rows = self.k_rows.max(1);
        let rx_cells = ((KNOT_R_HI / self.k_cell_w).ceil() as i32).min(((k_cols as i32) - 1) / 2);
        let ry_cells = ((KNOT_R_HI / self.k_cell_h).ceil() as i32).min(((k_rows as i32) - 1) / 2);
        let inv_blend = 1.0 / (KNOT_R_HI - KNOT_R_LO);
        let fc = self.far_cols.max(1);
        let fr = self.far_rows.max(1);
        let has_knots = self.ak_count > 0;
        let has_knot_forces = has_knots && (g_sim > 0.0 || self.em_coupling > 0.0) && self.gravity_range > 0.0;
        let max_vdamp = 0.45 / self.sim_speed.max(0.1);

        for i in 0..count {
            let mut p = self.particles[i];
            p.prev_x = p.x;
            p.prev_y = p.y;
            if p.polar_jet_age > 0.0 {
                p.polar_jet_age = (p.polar_jet_age - 0.015 * self.sim_speed).max(0.0);
            }

            if self.trapped_mode == 2 && p.trapped_in >= 0 {
                p.trapped_in = -1;
            }

            if p.trapped_in >= 0 && self.trapped_mode != 2 {
                // Trapped orbital dynamics inside a dynamically-warped spacetime horizon pocket
                let k_idx = p.trapped_in as usize;
                if k_idx < MAX_KNOTS && self.knots[k_idx].active {
                    let k = self.knots[k_idx];
                    let dx = torus_delta(p.x - k.x, w_span);
                    let dy = torus_delta(p.y - k.y, h_span);
                    let dist = (dx * dx + dy * dy).sqrt().max(0.05);
                    let nx_in = dx / dist;
                    let ny_in = dy / dist;

                    let mut u_loc = 0.0f32;
                    let mut ax_pauli = 0.0f32;
                    let mut ay_pauli = 0.0f32;
                    if cols > 2 && rows > 2 && grid_len > 0 {
                        let fx = clamp(p.x / dx_g, 0.0, (cols - 1) as f32 - 0.0001);
                        let fy = clamp(p.y / dy_g, 0.0, (rows - 1) as f32 - 0.0001);
                        let gc = fx.floor() as usize;
                        let gr = fy.floor() as usize;
                        let tx = fx - gc as f32;
                        let ty = fy - gr as f32;
                        let i00 = gc * rows + gr;
                        let i10 = (gc + 1) * rows + gr;
                        let i01 = gc * rows + (gr + 1);
                        let i11 = (gc + 1) * rows + (gr + 1);
                        if i11 < grid_len {
                            u_loc = (1.0 - tx) * (1.0 - ty) * self.grid[i00].u
                                + tx * (1.0 - ty) * self.grid[i10].u
                                + (1.0 - tx) * ty * self.grid[i01].u
                                + tx * ty * self.grid[i11].u;
                            let n00 = self.grid[i00].num_dens;
                            let n10 = self.grid[i10].num_dens;
                            let n01 = self.grid[i01].num_dens;
                            let n11 = self.grid[i11].num_dens;
                            let n_loc = (1.0 - tx) * (1.0 - ty) * n00 + tx * (1.0 - ty) * n10 + (1.0 - tx) * ty * n01 + tx * ty * n11;
                            if n_loc > n_pauli {
                                let excess = n_loc - n_pauli;
                                let dn_dx = (n10 - n00) * (1.0 - ty) + (n11 - n01) * ty;
                                let dn_dy = (n01 - n00) * (1.0 - tx) + (n11 - n10) * tx;
                                let p_grad = 0.09 * excess * (1.0 + excess * 0.04);
                                ax_pauli = (-dn_dx * p_grad).clamp(-4.0, 4.0);
                                ay_pauli = (-dn_dy * p_grad).clamp(-4.0, 4.0);
                            }
                        }
                    }

                    let ux_axis = k.axis_angle.cos();
                    let uy_axis = k.axis_angle.sin();
                    let tidal = self.tidal_pull[k_idx];
                    let tdx = self.tidal_dx[k_idx];
                    let tdy = self.tidal_dy[k_idx];
                    let bp_cos = fast_cos(k.braid_phase);
                    let bp_sin = fast_sin(k.braid_phase);
                    let (r_eff, sin_th, proj) = warped_horizon(
                        k.cloak_radius,
                        ux_axis, uy_axis,
                        nx_in, ny_in,
                        k.angular_vel,
                        bp_cos, bp_sin,
                        tidal, tdx, tdy,
                        u_loc,
                    );

                    let r_shell = r_eff * (0.28 + 0.46 * (((i % 7) as f32) / 6.0));
                    let radial_f = if dist < r_shell {
                        0.12 * (1.0 - dist / r_shell.max(0.5))
                    } else {
                        -0.045
                    };
                    let chiral = if (i & 1) == 0 { 1.0 } else { -1.0 };
                    let mut ax = nx_in * radial_f - (-ny_in) * (k.angular_vel * 0.024 * chiral) + ax_pauli;
                    let mut ay = ny_in * radial_f - nx_in * (k.angular_vel * 0.024 * chiral) + ay_pauli;
                    p.phase += (0.08 + freq_pos.max(0.3) * 0.04) * self.sim_speed;
                    let q_ang = p.phase + (i as f32) * 2.399963;
                    ax += q_ang.cos() * 0.036 * (1.0 + freq_pos * 0.06).min(1.6);
                    ay += q_ang.sin() * 0.036 * (1.0 + freq_pos * 0.06).min(1.6);

                    // Companion tidal shear stretches outer trapped particles into streams
                    let mut tidal_stripped = false;
                    if tidal > 0.04 {
                        let r_frac = (dist / r_eff.max(1.0)).clamp(0.0, 1.3);
                        let stretch = (2.0 * proj * proj - 0.5) * tidal * 0.16 * r_frac;
                        ax += nx_in * stretch + tdx * tidal * 0.05 * r_frac;
                        ay += ny_in * stretch + tdy * tidal * 0.05 * r_frac;
                        if dist >= r_eff * 0.80 && tidal > 0.22 && proj.abs() > 0.42 {
                            tidal_stripped = true;
                        }
                    }

                    // Soft horizon membrane restoring pocket (smooth gradient, zero circular snap)
                    let gate_t = smooth_pole_gate(sin_th, sin_psi);
                    let wall_hold = 1.0 - gate_t * (if self.trapped_mode == 0 { 0.92 } else { 0.35 });
                    if dist > r_eff * 0.78 && wall_hold > 0.02 {
                        let over = clamp((dist - r_eff * 0.78) / (r_eff * 0.42).max(1.0), 0.0, 2.0);
                        let restore = 0.24 * over * (1.0 + over) * wall_hold;
                        ax -= nx_in * restore;
                        ay -= ny_in * restore;
                        let rvx = p.vx - k.vx;
                        let rvy = p.vy - k.vy;
                        let v_rad = rvx * nx_in + rvy * ny_in;
                        if v_rad > 0.0 {
                            let damp_rad = v_rad * 0.28 * wall_hold;
                            ax -= nx_in * damp_rad;
                            ay -= ny_in * damp_rad;
                        }
                    }

                    p.vx = (p.vx + ax * self.sim_speed) * 0.990;
                    p.vy = (p.vy + ay * self.sim_speed) * 0.990;
                    let sp = (p.vx * p.vx + p.vy * p.vy).sqrt();
                    let cap_in = (v_c * 0.50).max(1.0);
                    if sp > cap_in {
                        p.vx *= cap_in / sp;
                        p.vy *= cap_in / sp;
                    }
                    p.x += p.vx * self.sim_speed;
                    p.y += p.vy * self.sim_speed;

                    if tidal_stripped {
                        p.trapped_in = -1;
                        trapped_counts[k_idx] -= 1;
                    } else {
                        let dx2 = torus_delta(p.x - k.x, w_span);
                        let dy2 = torus_delta(p.y - k.y, h_span);
                        let d2 = (dx2 * dx2 + dy2 * dy2).sqrt().max(0.01);
                        let nx = dx2 / d2;
                        let ny = dy2 / d2;
                        let sin_th2 = (nx * (-uy_axis) + ny * ux_axis).abs();
                        let gate_t2 = smooth_pole_gate(sin_th2, sin_psi);
                        let max_trapped = ((k.mass * 1.1).clamp(10.0, 32.0) * k.folding_progress) as i32 + k.trapped_cap_bonus;

                        let polar_escape = self.trapped_mode == 0 && gate_t2 > 0.58 && d2 >= r_eff * 0.95;
                        let cap_escape = trapped_counts[k_idx] > max_trapped && d2 >= r_eff * 0.92;
                        let far_escape = d2 >= r_eff * 1.48;

                        if polar_escape || cap_escape || far_escape {
                            p.trapped_in = -1;
                            trapped_counts[k_idx] -= 1;
                            if polar_escape {
                                let jet_s = (0.95 + k.angular_vel.abs() * 0.20).min(v_c * 0.55);
                                p.vx = lerp(p.vx, k.vx + nx * jet_s, 0.65);
                                p.vy = lerp(p.vy, k.vy + ny * jet_s, 0.65);
                                p.polar_jet_age = 0.85;
                            }
                        }
                    }

                    if p.x < 0.0 || p.x >= w_span {
                        p.x = p.x.rem_euclid(w_span);
                        p.prev_x = p.x;
                    }
                    if p.y < 0.0 || p.y >= h_span {
                        p.y = p.y.rem_euclid(h_span);
                        p.prev_y = p.y;
                    }

                    if p.trapped_in >= 0 {
                        let v2 = p.vx * p.vx + p.vy * p.vy;
                        let gamma = 1.0 / (1.0 - (v2 / v_c2).min(0.97)).sqrt();
                        let (mr, mg, mb) = mass_energy_color(p.freq_mult * gamma);
                        p.r = lerp(mr, 0.96, 0.55);
                        p.g = lerp(mg, 0.88, 0.55);
                        p.b = lerp(mb, 0.62, 0.55);
                        self.particles[i] = p;
                        continue;
                    }
                } else {
                    p.trapped_in = -1;
                }
            }

            // Free Field Forces (strictly 0 when frequency == 0, gravity == 0, and no attractor/dark_energy!)
            let mut ax = 0.0f32;
            let mut ay = 0.0f32;

            let v2_cur = p.vx * p.vx + p.vy * p.vy;
            let gamma_cur = 1.0 / (1.0 - (v2_cur / v_c2).min(0.97)).sqrt();
            let m_rel = (p.freq_mult * gamma_cur).max(0.25);

            // Radiative cooling of any collisional internal excitation back to rest base_freq_mult
            p.freq_mult = lerp(p.freq_mult, p.base_freq_mult, (0.045 * self.sim_speed).min(0.45));

            // 1. Frequency-driven quantum wave-bath relaxation + Vacuum Metric geometry (strictly 0 at freq=0!)
            if freq_pos > 0.0 {
                let p_freq = freq_pos * p.base_freq_mult;
                p.phase = (p.phase + (0.04 + p_freq * 0.03) * self.sim_speed) % (PI * 2048.0);
                let v_eq = v_c * 0.52 * (0.072 * (freq_pos / p.freq_mult.max(0.25)).sqrt()).tanh();

                let s = 0.03;
                let ph1 = (p.x * 0.8 + p.y * 0.6) * s - t_wave;
                let ph2 = (-p.x * 0.5 + p.y * 0.86) * s + t_wave * 1.2;
                let wx = (fast_sin(p.phase) * 0.55 + fast_sin(ph1) * 0.45) * v_eq;
                let wy = (fast_cos(p.phase * 1.3) * 0.55 + fast_cos(ph2) * 0.45) * v_eq;
                let bath_rate = (1.0 - (-0.095 * self.sim_speed).exp()) / self.sim_speed.max(0.05);
                ax += (wx - p.vx) * bath_rate;
                ay += (wy - p.vy) * bath_rate;

                match self.medium_id {
                    0 => {
                        let k_f = 0.036;
                        let phi_a = (p.x * 0.866 + p.y * 0.500) * k_f - t_wave * 0.42;
                        let phi_b = (-p.x * 0.500 + p.y * 0.866) * k_f + t_wave * 0.34;
                        let flow = (fast_sin(phi_a) + fast_cos(phi_b)) * 0.015 * (freq_pos / 4.0).clamp(0.15, 1.4);
                        ax += (fast_cos(phi_a) * 0.500 - fast_sin(phi_b) * 0.866) * flow;
                        ay += (fast_cos(phi_a) * (-0.866) - fast_sin(phi_b) * 0.500) * flow;
                    }
                    1 => {
                        for &(vx_c, vy_c, sign) in &[
                            (self.width * 0.35, self.height * 0.42, 1.0f32),
                            (self.width * 0.65, self.height * 0.58, -1.0f32),
                        ] {
                            let dx = p.x - vx_c;
                            let dy = p.y - vy_c;
                            let dist = (dx * dx + dy * dy).sqrt().max(1.0);
                            if dist < 240.0 {
                                let rot = ((sign * 1.6 * freq_pos.sqrt() * 0.25) / (dist * 0.015 + 1.5))
                                    .clamp(-v_c * 0.45, v_c * 0.45);
                                ax += ((-dy / dist) * rot - p.vx) * 0.06;
                                ay += ((dx / dist) * rot - p.vy) * 0.06;
                            }
                        }
                    }
                    _ => {}
                }
            }

            // 2. 2D Wave Sheet Surfing (-grad u, periodic bilinearly sampled) & Dent Tracking
            let mut u_loc = 0.0f32;
            if cols > 2 && rows > 2 && grid_len > 0 {
                let fx = clamp(p.x / dx_g, 0.0, (cols - 1) as f32 - 0.0001);
                let fy = clamp(p.y / dy_g, 0.0, (rows - 1) as f32 - 0.0001);
                let gc = fx.floor() as usize;
                let gr = fy.floor() as usize;
                let tx = fx - gc as f32;
                let ty = fy - gr as f32;
                let w00 = (1.0 - tx) * (1.0 - ty);
                let w10 = tx * (1.0 - ty);
                let w01 = (1.0 - tx) * ty;
                let w11 = tx * ty;
                let i00 = gc * rows + gr;
                let i10 = (gc + 1) * rows + gr;
                let i01 = gc * rows + (gr + 1);
                let i11 = (gc + 1) * rows + (gr + 1);
                if i11 < grid_len {
                    u_loc = w00 * self.grid[i00].u + w10 * self.grid[i10].u + w01 * self.grid[i01].u + w11 * self.grid[i11].u;
                    p.dent = p.dent * 0.96 + (-u_loc).max(0.0) * 0.04;
                    let dux = clamp(((self.grid[i10].u - self.grid[i00].u) * (1.0 - ty) + (self.grid[i11].u - self.grid[i01].u) * ty) * (10.0 / dx_g), -1.8, 1.8);
                    let duy = clamp(((self.grid[i01].u - self.grid[i00].u) * (1.0 - tx) + (self.grid[i11].u - self.grid[i10].u) * tx) * (10.0 / dy_g), -1.8, 1.8);
                    ax += -dux * 0.045;
                    ay += -duy * 0.045;
                }
            }

            // 2b. O(N) Pauli Exclusion / Fermi Degeneracy Pressure
            if cols > 2 && rows > 2 && grid_len > 0 {
                let fx = clamp(p.x / dx_g, 0.0, (cols - 1) as f32 - 0.0001);
                let fy = clamp(p.y / dy_g, 0.0, (rows - 1) as f32 - 0.0001);
                let gc = fx.floor() as usize;
                let gr = fy.floor() as usize;
                let tx = fx - gc as f32;
                let ty = fy - gr as f32;
                let i00 = gc * rows + gr;
                let i10 = (gc + 1) * rows + gr;
                let i01 = gc * rows + (gr + 1);
                let i11 = (gc + 1) * rows + (gr + 1);
                if i11 < grid_len {
                    let n00 = self.grid[i00].num_dens;
                    let n10 = self.grid[i10].num_dens;
                    let n01 = self.grid[i01].num_dens;
                    let n11 = self.grid[i11].num_dens;
                    let n_loc = (1.0 - tx) * (1.0 - ty) * n00 + tx * (1.0 - ty) * n10 + (1.0 - tx) * ty * n01 + tx * ty * n11;
                    if n_loc > n_pauli {
                        let excess = n_loc - n_pauli;
                        let dn_dx = (n10 - n00) * (1.0 - ty) + (n11 - n01) * ty;
                        let dn_dy = (n01 - n00) * (1.0 - tx) + (n11 - n10) * tx;
                        let p_grad = (0.14 * excess * (1.0 + excess * 0.06)) / m_rel;
                        ax += (-dn_dx * p_grad).clamp(-12.0, 12.0);
                        ay += (-dn_dy * p_grad).clamp(-12.0, 12.0);

                        let zpf_ang = p.phase + (i as f32) * 2.399963 + self.time * 1.3;
                        let a_zpf = (0.095 * excess.sqrt() * (1.0 + excess * 0.025) / m_rel).min(6.0);
                        ax += fast_cos(zpf_ang) * a_zpf;
                        ay += fast_sin(zpf_ang) * a_zpf;
                    }
                }
            }

            // 3. U(1) Electromagnetic Lorentz Force (bilinearly sampled)
            if cols > 2 && rows > 2 && p.charge != 0.0 && grid_len > 0 {
                let fx = clamp(p.x / dx_g, 0.0, (cols - 1) as f32 - 0.0001);
                let fy = clamp(p.y / dy_g, 0.0, (rows - 1) as f32 - 0.0001);
                let gc = fx.floor() as usize;
                let gr = fy.floor() as usize;
                let tx = fx - gc as f32;
                let ty = fy - gr as f32;
                let w00 = (1.0 - tx) * (1.0 - ty);
                let w10 = tx * (1.0 - ty);
                let w01 = (1.0 - tx) * ty;
                let w11 = tx * ty;
                let i00 = gc * rows + gr;
                let i10 = (gc + 1) * rows + gr;
                let i01 = gc * rows + (gr + 1);
                let i11 = (gc + 1) * rows + (gr + 1);
                if i11 < grid_len {
                    let g00 = self.grid[i00];
                    let g10 = self.grid[i10];
                    let g01 = self.grid[i01];
                    let g11 = self.grid[i11];
                    let ex = w00 * g00.ex + w10 * g10.ex + w01 * g01.ex + w11 * g11.ex;
                    let ey = w00 * g00.ey + w10 * g10.ey + w01 * g01.ey + w11 * g11.ey;
                    let bz = w00 * g00.bz + w10 * g10.bz + w01 * g01.bz + w11 * g11.bz;
                    let q_over_m = p.charge / m_rel;
                    ax += q_over_m * (ex + p.vy * bz) * 0.30;
                    ay += q_over_m * (ey - p.vx * bz) * 0.30;
                }
            }

            // 4. Shockwaves (only kinetic explosions push particles radially; black-hole mergers emit pure metric ripples)
            for sw in self.shockwaves.iter() {
                if !sw.active || sw.kinetic_push <= 0.0 { continue; }
                let dx = torus_delta(p.x - sw.x, w_span);
                let dy = torus_delta(p.y - sw.y, h_span);
                let d2 = dx * dx + dy * dy;
                let r_lim = sw.radius + 50.0;
                if d2 < r_lim * r_lim {
                    let d = d2.sqrt().max(1.0);
                    let bf = (sw.kinetic_push * 2.5) / ((d + 28.0) * m_rel) * (1.0 - sw.age / sw.max_age);
                    ax += (dx / d) * bf;
                    ay += (dy / d) * bf;
                }
            }

            // 5. P3M Knot Forces & Dynamically-Warped Spacetime Horizon (single near-field pass)
            let mut grav_glow = 0.0f32;
            let inv_vortex_scale = 1.0 / vortex_scale;
            if has_knots {
                let mut dip_bx = 0.0f32;
                let mut dip_by = 0.0f32;

                if has_knot_forces && self.use_far_field {
                    let u = (p.x / self.far_dx).rem_euclid(fc as f32);
                    let v = (p.y / self.far_dy).rem_euclid(fr as f32);
                    let c0 = (u.floor() as usize).min(fc - 1);
                    let c1 = if c0 + 1 == fc { 0 } else { c0 + 1 };
                    let r0 = (v.floor() as usize).min(fr - 1);
                    let r1 = if r0 + 1 == fr { 0 } else { r0 + 1 };
                    let tx = u - c0 as f32;
                    let ty = v - r0 as f32;
                    let w00 = (1.0 - tx) * (1.0 - ty);
                    let w10 = tx * (1.0 - ty);
                    let w01 = (1.0 - tx) * ty;
                    let w11 = tx * ty;

                    let f00 = self.far_grid[c0 * fr + r0];
                    let f10 = self.far_grid[c1 * fr + r0];
                    let f01 = self.far_grid[c0 * fr + r1];
                    let f11 = self.far_grid[c1 * fr + r1];

                    let vdamp = (w00 * f00.vdamp + w10 * f10.vdamp + w01 * f01.vdamp + w11 * f11.vdamp).min(max_vdamp);
                    ax += (w00 * f00.gx + w10 * f10.gx + w01 * f01.gx + w11 * f11.gx) - p.vx * vdamp;
                    ay += (w00 * f00.gy + w10 * f10.gy + w01 * f01.gy + w11 * f11.gy) - p.vy * vdamp;
                    if p.charge != 0.0 && self.em_coupling > 0.0 {
                        dip_bx += w00 * f00.bx + w10 * f10.bx + w01 * f01.bx + w11 * f11.bx;
                        dip_by += w00 * f00.by + w10 * f10.by + w01 * f01.by + w11 * f11.by;
                    }
                    let fg = w00 * f00.closeness + w10 * f10.closeness + w01 * f01.closeness + w11 * f11.closeness;
                    if fg > grav_glow { grav_glow = fg; }
                }

                // Near-field exact evaluation (dist < KNOT_R_HI = 58.0) via O(1) cell lookup
                let pc = ((p.x / self.k_cell_w).floor() as i32).rem_euclid(k_cols as i32);
                let pr = ((p.y / self.k_cell_h).floor() as i32).rem_euclid(k_rows as i32);
                for dc in -rx_cells..=rx_cells {
                    let cc = (pc + dc).rem_euclid(k_cols as i32) as usize;
                    for dr in -ry_cells..=ry_cells {
                        let cr = (pr + dr).rem_euclid(k_rows as i32) as usize;
                        let mut ai = self.k_cell_head[cc * k_rows + cr];
                        while ai >= 0 {
                            let ak = self.ak_buf[ai as usize];
                            let dx = torus_delta(ak.x - p.x, w_span);
                            let dy = torus_delta(ak.y - p.y, h_span);
                            let d2 = dx * dx + dy * dy;
                            let cut = ak.end_d.min(KNOT_R_HI);
                            let h_cut = ak.cloak_radius * 1.60;

                            if has_knot_forces && d2 < cut * cut {
                                let dist = d2.sqrt().max(1.0);
                                let w_near = if !self.use_far_field || dist <= KNOT_R_LO {
                                    1.0
                                } else {
                                    let s = (dist - KNOT_R_LO) * inv_blend;
                                    1.0 - s * s * (3.0 - 2.0 * s)
                                };
                                let rf = if dist <= ak.start_d {
                                    1.0
                                } else {
                                    let t = clamp((dist - ak.start_d) / (ak.end_d - ak.start_d).max(1.0), 0.0, 1.0);
                                    1.0 - t * t * (3.0 - 2.0 * t)
                                };
                                let w_eff = rf * w_near;
                                if w_eff > 0.0001 {
                                    let ng = clamp(1.0 - (dist / ak.cloak_radius.max(1.0) - 1.0) * 0.25, 0.0, 1.0) * w_near;
                                    if ng > grav_glow { grav_glow = ng; }

                                    let inv_d = 1.0 / dist;
                                    let ux_kp = dx * inv_d;
                                    let uy_kp = dy * inv_d;
                                    let rx = -dx;
                                    let ry = -dy;

                                    if g_sim > 0.0 {
                                        let rvx = p.vx - ak.vx;
                                        let rvy = p.vy - ak.vy;
                                        let l_ang = rx * rvy - ry * rvx;
                                        let gr_corr = 1.0 + ((3.0 * l_ang * l_ang) / (c2 * (d2 + 25.0))).min(0.65);
                                        let f_g = ((g_sim * ak.mass * ak.grip * 0.075) / (d2 + 36.0) * gr_corr * w_eff).min(0.45);

                                        if dist < ak.radius * 0.92 {
                                            let core_rep = 0.44 * (1.0 - dist / (ak.radius * 0.92).max(1.0));
                                            ax -= ux_kp * core_rep;
                                            ay -= uy_kp * core_rep;
                                        } else {
                                            ax += ux_kp * f_g * ak.f_scale;
                                            ay += uy_kp * f_g * ak.f_scale;
                                        }

                                        let spin_blend = 0.06 * w_eff * (0.15 + 0.85 * ak.folding_progress);
                                        if spin_blend > 0.00001 {
                                            let x_att = dist * inv_vortex_scale;
                                            let atten = 1.0 / (1.0 + x_att + 0.5 * x_att * x_att);
                                            let v_drag = ((ak.j_spin_drive * atten) / (c2 * (d2 + 28.0)))
                                                .clamp(-v_c * 0.48, v_c * 0.48);
                                            let tx = uy_kp;
                                            let ty = -ux_kp;
                                            ax += (tx * v_drag - rvx) * spin_blend;
                                            ay += (ty * v_drag - rvy) * spin_blend;
                                        }
                                        if dist < ak.cloak_radius * 2.2 {
                                            let delta_w = l_ang / (d2 + 28.0) - ak.angular_vel * 0.12;
                                            self.spin_acc[ak.idx] += delta_w * 0.012 * m_rel * w_eff * self.sim_speed;
                                        }
                                    }

                                    if p.charge != 0.0 && self.em_coupling > 0.0 {
                                        let r_soft = dist + 14.0;
                                        let inv_rs = 1.0 / r_soft;
                                        let rx_u = rx * inv_rs;
                                        let ry_u = ry * inv_rs;
                                        let md = ak.mx * rx_u + ak.my * ry_u;
                                        let d3_inv = inv_rs * inv_rs * inv_rs;
                                        let scale_b = ak.mu_k * d3_inv * w_near;
                                        dip_bx += (3.0 * md * rx_u - ak.mx) * scale_b;
                                        dip_by += (3.0 * md * ry_u - ak.my) * scale_b;
                                    }
                                }
                            }

                            // Soft, dynamically-warped outer spacetime horizon barrier + polar throat capture
                            if !ak.is_quark && ak.folding_progress >= 0.45 && d2 < h_cut * h_cut {
                                let dist = d2.sqrt().max(0.5);
                                let inv_d = 1.0 / dist;
                                let rx_u = -dx * inv_d;
                                let ry_u = -dy * inv_d;
                                let (r_eff, sin_th, _) = warped_horizon(
                                    ak.cloak_radius,
                                    ak.ux, ak.uy,
                                    rx_u, ry_u,
                                    ak.angular_vel,
                                    ak.bp_cos, ak.bp_sin,
                                    ak.tidal_pull, ak.tidal_dx, ak.tidal_dy,
                                    u_loc,
                                );
                                let gate_t = smooth_pole_gate(sin_th, sin_psi);
                                let h_form = clamp((ak.folding_progress - 0.45) / 0.55, 0.0, 1.0);
                                let barrier_w = (1.0 - gate_t) * h_form * h_form;

                                if barrier_w > 0.02 && dist < r_eff * 1.18 {
                                    let u_depth = clamp((r_eff * 1.18 - dist) / (r_eff * 0.56).max(1.0), 0.0, 1.25);
                                    let f_mem = barrier_w * (u_depth * u_depth) * 0.62;
                                    ax += rx_u * f_mem;
                                    ay += ry_u * f_mem;

                                    let rvx = p.vx - ak.vx;
                                    let rvy = p.vy - ak.vy;
                                    let v_rad = rvx * rx_u + rvy * ry_u;
                                    if v_rad < 0.0 {
                                        let damp_in = (-v_rad) * barrier_w * u_depth * 0.40;
                                        ax += rx_u * damp_in;
                                        ay += ry_u * damp_in;
                                    }
                                }

                                if p.trapped_in < 0
                                    && self.trapped_mode != 2
                                    && p.polar_jet_age <= 0.0
                                    && gate_t > 0.52
                                    && dist <= r_eff * 0.90
                                {
                                    let k_idx = ak.idx;
                                    let max_trapped = ((ak.mass * 1.1).clamp(10.0, 32.0) * ak.folding_progress) as i32
                                        + self.knots[k_idx].trapped_cap_bonus;
                                    let fits_wavelength = (p.freq_mult * freq_pos.max(0.6)) >= (10.0 / ak.cloak_radius.max(3.0));
                                    if trapped_counts[k_idx] < max_trapped && (self.trapped_mode == 1 || fits_wavelength) {
                                        p.trapped_in = k_idx as i32;
                                        trapped_counts[k_idx] += 1;
                                        let l_cap = (-dx) * (p.vy - ak.vy) - (-dy) * (p.vx - ak.vx);
                                        let delta_w_cap = l_cap / (d2 + 28.0) - ak.angular_vel * 0.12;
                                        self.spin_acc[k_idx] += delta_w_cap * 0.14 * m_rel;
                                    }
                                }
                            }

                            ai = self.k_cell_next[ai as usize];
                        }
                    }
                }

                if has_knot_forces && p.charge != 0.0 && self.em_coupling > 0.0 {
                    let steer = (p.charge / m_rel) * self.em_coupling * 2600.0;
                    ax += (dip_bx * steer).clamp(-0.40, 0.40);
                    ay += (dip_by * steer).clamp(-0.40, 0.40);
                }
            }

            // 6. Interactive Mouse Attractor (Plummer core-softened)
            if self.attr_active {
                let dx = self.attr_x - p.x;
                let dy = self.attr_y - p.y;
                let dist = (dx * dx + dy * dy).sqrt().max(0.5);
                if dist < self.gravity_range * 1.5 {
                    let g_pull = g_sim.max(12.0);
                    let r_core = 18.0f32;
                    let f = ((self.attr_strength * g_pull * 0.055 * dist) / (dist * dist * 0.02 + r_core * r_core)).min(3.5);
                    ax += (dx / dist) * f;
                    ay += (dy / dist) * f;
                    if self.orbital_boost > 0.0 {
                        let swirl = self.orbital_boost * 0.015 * (dist / (dist + 10.0));
                        ax += (-dy / dist) * swirl;
                        ay += (dx / dist) * swirl;
                    }
                }
            }

            // 7. Cosmic Expansion
            if self.dark_energy > 0.0 {
                let dx = p.x - cx;
                let dy = p.y - cy;
                let dist = (dx * dx + dy * dy).sqrt().max(1.0);
                let hubble = self.dark_energy * dist * 0.0003;
                ax += (dx / dist) * hubble;
                ay += (dy / dist) * hubble;
            }

            // Subluminal integration with full NaN & wrapping protection
            p.vx = (p.vx + ax * self.sim_speed) * damp;
            p.vy = (p.vy + ay * self.sim_speed) * damp;
            if !p.vx.is_finite() || !p.vy.is_finite() {
                p.vx = 0.0;
                p.vy = 0.0;
            }
            let v_len = (p.vx * p.vx + p.vy * p.vy).sqrt();
            let max_v = v_c * 0.985;
            if v_len > max_v {
                p.vx *= max_v / v_len;
                p.vy *= max_v / v_len;
            }
            p.x += p.vx * self.sim_speed;
            p.y += p.vy * self.sim_speed;

            if !p.x.is_finite() || !p.y.is_finite() {
                p.x = cx;
                p.y = cy;
                p.prev_x = cx;
                p.prev_y = cy;
            }
            if p.x < 0.0 || p.x > self.width {
                p.x = p.x.rem_euclid(self.width.max(1.0));
                p.prev_x = p.x;
            }
            if p.y < 0.0 || p.y > self.height {
                p.y = p.y.rem_euclid(self.height.max(1.0));
                p.prev_y = p.y;
            }

            let v2_new = p.vx * p.vx + p.vy * p.vy;
            let gamma_new = 1.0 / (1.0 - (v2_new / v_c2).min(0.97)).sqrt();
            let grav_boost = clamp(grav_glow, 0.0, 1.0) * 0.30;
            let (mut cr, mut cg, mut cb) = mass_energy_color(p.freq_mult * gamma_new + grav_boost);

            if p.polar_jet_age > 0.0 {
                let jt = p.polar_jet_age * 0.80;
                cr = lerp(cr, 1.00, jt);
                cg = lerp(cg, 0.89, jt);
                cb = lerp(cb, 0.55, jt);
            }
            p.r = cr;
            p.g = cg;
            p.b = cb;
            self.particles[i] = p;
        }
    }

    fn step_collisions(&mut self) {
        let count = self.particle_count.min(MAX_PARTICLES);
        if !self.intra_collisions || count <= 1 { return; }

        for h in self.head.iter_mut() { *h = -1; }
        let cell_sz = 10.0f32;
        for i in 0..count {
            if self.particles[i].trapped_in >= 0 { continue; }
            let gx = (self.particles[i].x / cell_sz) as i32;
            let gy = (self.particles[i].y / cell_sz) as i32;
            let h = (((gx.wrapping_mul(73856093)) ^ (gy.wrapping_mul(19349663))) as usize) & (HASH_BUCKETS - 1);
            self.next[i] = self.head[h];
            self.head[h] = i as i32;
        }

        let w_span = self.width.max(1.0);
        let h_span = self.height.max(1.0);
        let shear_thresh = 0.65;
        for i in 0..count {
            if self.particles[i].trapped_in >= 0 { continue; }
            let gx = (self.particles[i].x / cell_sz) as i32;
            let gy = (self.particles[i].y / cell_sz) as i32;
            let h = (((gx.wrapping_mul(73856093)) ^ (gy.wrapping_mul(19349663))) as usize) & (HASH_BUCKETS - 1);
            let mut j = self.head[h];
            let mut checks = 0;
            while j >= 0 && checks < 18 {
                let ju = j as usize;
                if ju > i {
                    let dx = self.particles[ju].x - self.particles[i].x;
                    let dy = self.particles[ju].y - self.particles[i].y;
                    let d2 = dx * dx + dy * dy;
                    let min_d = (self.particles[i].size + self.particles[ju].size) * 0.55;
                    if d2 < min_d * min_d {
                        let (d, nx, ny) = if d2 > 0.0004 {
                            let dist = d2.sqrt();
                            (dist, dx / dist, dy / dist)
                        } else {
                            // Break exact coordinate overlaps cleanly via golden angle instead of leaving them stuck
                            let ang = fast_hash((i as u32) ^ ((ju as u32).wrapping_mul(2654435761))) * PI * 2.0;
                            (0.02, ang.cos(), ang.sin())
                        };

                        let m1 = self.particles[i].freq_mult.max(0.25);
                        let m2 = self.particles[ju].freq_mult.max(0.25);
                        let mtot = m1 + m2;
                        let overlap = (min_d - d) * 0.42;
                        self.particles[i].x = (self.particles[i].x - nx * overlap * (m2 / mtot)).rem_euclid(w_span);
                        self.particles[i].y = (self.particles[i].y - ny * overlap * (m2 / mtot)).rem_euclid(h_span);
                        self.particles[ju].x = (self.particles[ju].x + nx * overlap * (m1 / mtot)).rem_euclid(w_span);
                        self.particles[ju].y = (self.particles[ju].y + ny * overlap * (m1 / mtot)).rem_euclid(h_span);

                        let rvx = self.particles[ju].vx - self.particles[i].vx;
                        let rvy = self.particles[ju].vy - self.particles[i].vy;
                        let vn = rvx * nx + rvy * ny;
                        if vn < 0.0 {
                            let inv_sum = 1.0 / m1 + 1.0 / m2;
                            let e_rest = 0.94f32;
                            let j_imp = -((1.0 + e_rest) * vn) / inv_sum;
                            self.particles[i].vx -= (j_imp / m1) * nx;
                            self.particles[i].vy -= (j_imp / m1) * ny;
                            self.particles[ju].vx += (j_imp / m2) * nx;
                            self.particles[ju].vy += (j_imp / m2) * ny;

                            let mu = (m1 * m2) / (m1 + m2);
                            let lost_ke = 0.5 * mu * vn * vn * (1.0 - e_rest * e_rest);
                            if lost_ke > 0.03 {
                                let heat = (lost_ke * 0.012).min(0.04);
                                self.particles[i].freq_mult = (self.particles[i].freq_mult + heat)
                                    .min(self.particles[i].base_freq_mult + 0.25);
                                self.particles[ju].freq_mult = (self.particles[ju].freq_mult + heat)
                                    .min(self.particles[ju].base_freq_mult + 0.25);
                            }

                            let shear = nx * rvy - ny * rvx;
                            if self.frequency > 0.8
                                && shear.abs() > shear_thresh
                                && self.particles[i].twist_thirds == 0
                                && self.particles[ju].twist_thirds == 0
                            {
                                let sign: i8 = if shear > 0.0 { 1 } else { -1 };
                                self.particles[i].twist_thirds = sign;
                                self.particles[i].charge = sign as f32 / 3.0;
                                self.particles[ju].twist_thirds = -sign;
                                self.particles[ju].charge = -sign as f32 / 3.0;
                            } else if (self.particles[i].twist_thirds as i32) * (self.particles[ju].twist_thirds as i32) < 0
                                && shear.abs() < 0.45
                            {
                                let step_i = if self.particles[i].twist_thirds > 0 { -1 } else { 1 };
                                self.particles[i].twist_thirds += step_i;
                                self.particles[i].charge = self.particles[i].twist_thirds as f32 / 3.0;
                                self.particles[ju].twist_thirds -= step_i;
                                self.particles[ju].charge = self.particles[ju].twist_thirds as f32 / 3.0;
                            }
                        }
                    }
                }
                j = self.next[ju];
                checks += 1;
            }
        }
    }

    fn step_metric_grid(&mut self, g_sim: f32) {
        let cols = self.grid_cols;
        let rows = self.grid_rows;
        let grid_len = (cols * rows).min(MAX_GRID_NODES);
        if cols <= 2 || rows <= 2 { return; }

        let dx_g = self.grid_dx.max(1.0);
        let dy_g = self.grid_dy.max(1.0);
        let spacing = self.grid_spacing.max(1.0);
        let cx = self.width * 0.5;
        let cy = self.height * 0.5;
        let freq_pos = self.frequency.max(0.0);
        let w_span = self.width.max(1.0);
        let h_span = self.height.max(1.0);
        let t_stiff = (self.sheet_tension / 0.22).clamp(0.45, 2.6);
        let has_knots = g_sim > 0.0 && self.gravity_range > 0.0 && self.ak_count > 0;
        let g_norm = (g_sim / (6.6743 * 20.0)).clamp(0.0, 2.0);
        let range_norm = (self.gravity_range / 70.0).clamp(0.35, 2.2);

        // Pass 1: Isotropic 2D wave-sheet -grad(u) contraction, shockwave ripples, pointer funnel, & medium vortex
        for c in 0..cols {
            let cl = c.saturating_sub(1);
            let cr = (c + 1).min(cols - 1);
            for r in 0..rows {
                let idx = c * rows + r;
                if idx >= grid_len { continue; }
                let ox = self.grid[idx].ox;
                let oy = self.grid[idx].oy;
                let mut dx_tot = 0.0f32;
                let mut dy_tot = 0.0f32;
                let mut local_strain = 0.0f32;

                let ru = r.saturating_sub(1);
                let rd = (r + 1).min(rows - 1);
                let du_dx = (self.grid[cr * rows + r].u - self.grid[cl * rows + r].u) * 0.5;
                let du_dy = (self.grid[c * rows + rd].u - self.grid[c * rows + ru].u) * 0.5;
                let u_wave = self.grid[idx].u;
                dx_tot += -du_dx * 1.40;
                dy_tot += -du_dy * 1.40;
                let grad_u = (du_dx * du_dx + du_dy * du_dy).sqrt();
                local_strain = local_strain.max(clamp((-u_wave) * 0.14 + grad_u * 0.22, 0.0, 0.65));

                for sw in self.shockwaves.iter() {
                    if !sw.active { continue; }
                    let dx = torus_delta(ox - sw.x, w_span);
                    let dy = torus_delta(oy - sw.y, h_span);
                    let d2 = dx * dx + dy * dy;
                    let r_max = sw.radius + 32.0;
                    if d2 < r_max * r_max {
                        let dist = d2.sqrt().max(1.0);
                        let band = (dist - sw.radius).abs();
                        if band < 32.0 {
                            let s_w = (1.0 - band / 32.0) * (1.0 - sw.age / sw.max_age);
                            let ripple = fast_sin((dist - sw.radius) * 0.22) * s_w * 4.2;
                            dx_tot += (dx / dist) * (s_w * 3.0 + ripple);
                            dy_tot += (dy / dist) * (s_w * 3.0 + ripple);
                            local_strain = local_strain.max(s_w * 0.72);
                        }
                    }
                }

                if self.attr_active && self.gravity_range > 0.0 {
                    let dx = self.attr_x - ox;
                    let dy = self.attr_y - oy;
                    let d2 = dx * dx + dy * dy;
                    let r_attr = clamp(32.0 * range_norm.sqrt(), 24.0, 56.0);
                    let cut_attr = r_attr * 2.8;
                    if d2 < cut_attr * cut_attr {
                        let pull = clamp(self.attr_strength * 0.11, 0.15, 0.85)
                            * (-d2 / (2.0 * r_attr * r_attr)).exp();
                        dx_tot += dx * pull;
                        dy_tot += dy * pull;
                        local_strain = local_strain.max(clamp(pull * 0.75, 0.0, 0.85));
                    }
                }

                if self.medium_id == 1 && freq_pos > 0.0 {
                    let dx = cx - ox;
                    let dy = cy - oy;
                    let d2 = dx * dx + dy * dy;
                    if d2 < 28_900.0 {
                        let dist = d2.sqrt().max(1.0);
                        let twist = (1.0 - dist / 170.0) * clamp(freq_pos * 0.02, 0.0, 0.35);
                        dx_tot += -dy * twist;
                        dy_tot += dx * twist;
                    }
                }

                self.grid[idx].x = dx_tot;
                self.grid[idx].y = dy_tot;
                self.grid[idx].strain = local_strain;
            }
        }

        // Pass 2: Local spacetime metric deformation per active knot (toroidally tiled)
        if has_knots {
            for ai in 0..self.ak_count {
                let ak = self.ak_buf[ai];
                let r_well = clamp((11.0 + 3.4 * ak.mass.sqrt()) / t_stiff, 12.0, 48.0) * (0.72 + 0.28 * range_norm.sqrt());
                let cut = clamp(r_well * 2.8, 85.0, 145.0);
                let cut2 = cut * cut;
                let inv_cut2 = 1.0 / cut2;
                let inv_2r2 = 1.0 / (2.0 * r_well * r_well);
                let r_drag = r_well * 1.45;
                let inv_2rd2 = 1.0 / (2.0 * r_drag * r_drag);

                let fp_s = ak.folding_progress * ak.folding_progress * (3.0 - 2.0 * ak.folding_progress);
                let sv = clamp(ak.angular_vel * 0.28, -1.35, 1.35) * ak.axis_tilt_cb * fp_s;
                let sg2 = if ak.angular_vel < 0.0 { -1.0f32 } else { 1.0f32 };
                let a0_base = clamp(2.4 + ak.mass.sqrt() * 0.7, 2.4, 7.2)
                    * (0.3 + 0.7 * ak.axis_tilt_cb)
                    * fp_s
                    * g_norm.clamp(0.4, 1.5);
                let pinch_amp = ak.grip * 0.88 * ak.f_scale * g_norm.clamp(0.35, 1.25).min(0.92);

                let r_cloak = ak.cloak_radius.max(8.0);
                let braid_cut = r_cloak * 1.6;
                let ri_x = (cut / dx_g).ceil() as i32 + 1;
                let ri_y = (cut / dy_g).ceil() as i32 + 1;

                for &ox_tile in &[-w_span, 0.0f32, w_span] {
                    let kx_t = ak.x + ox_tile;
                    if kx_t + cut < 0.0 || kx_t - cut > w_span + dx_g { continue; }
                    for &oy_tile in &[-h_span, 0.0f32, h_span] {
                        let ky_t = ak.y + oy_tile;
                        if ky_t + cut < 0.0 || ky_t - cut > h_span + dy_g { continue; }
                        let gc = (kx_t / dx_g).round() as i32;
                        let gr = (ky_t / dy_g).round() as i32;
                        let c0 = (gc - ri_x).max(0) as usize;
                        let c1 = (gc + ri_x).min(cols as i32 - 1) as usize;
                        let r0 = (gr - ri_y).max(0) as usize;
                        let r1 = (gr + ri_y).min(rows as i32 - 1) as usize;

                        for c in c0..=c1 {
                            for rn in r0..=r1 {
                                let idx = c * rows + rn;
                                if idx >= grid_len { continue; }
                                let dx = kx_t - self.grid[idx].ox;
                                let dy = ky_t - self.grid[idx].oy;
                                let d2 = dx * dx + dy * dy;
                                if d2 >= cut2 || d2 < 0.01 { continue; }

                                let dist = d2.sqrt();
                                let inv_d = 1.0 / dist;
                                let edge_t = 1.0 - d2 * inv_cut2;
                                let env_edge = edge_t * edge_t;

                                // 1. Smooth Gaussian gravitational funnel pinch
                                let x_w = d2 * inv_2r2;
                                let gauss_w = 1.0 / (1.0 + x_w + 0.5 * x_w * x_w);
                                let pull = (pinch_amp * gauss_w * env_edge).min(0.90);
                                let mut px = dx * pull;
                                let mut py = dy * pull;

                                // 2. True 2D rotational frame-dragging of the vertex around knot center
                                let x_d = d2 * inv_2rd2;
                                let ph2 = sv * (1.0 / (1.0 + x_d + 0.5 * x_d * x_d)) * env_edge;
                                if ph2.abs() > 0.002 {
                                    let cq = fast_cos(ph2);
                                    let sn = fast_sin(ph2);
                                    px += -dx * (cq - 1.0) + dy * sn;
                                    py += -dx * sn - dy * (cq - 1.0);
                                }

                                // 3. m = 2 spin-2 spiral gravitational waves (zero-atan2 identity)
                                let inv_d2 = inv_d * inv_d;
                                let cos2t = (dx * dx - dy * dy) * inv_d2;
                                let sin2t = (2.0 * dx * dy) * inv_d2;
                                let x_gw = dist * (1.0 / 72.0);
                                let atten_gw = 1.0 / (1.0 + x_gw + 0.5 * x_gw * x_gw);
                                let alpha = 0.065 * dist * sg2 + 2.0 * ak.braid_phase;
                                let w2 = (a0_base * atten_gw * env_edge)
                                    * (sin2t * fast_cos(alpha) - cos2t * fast_sin(alpha));
                                px += (-dx * inv_d) * w2 + (dy * inv_d) * (w2 * 0.65);
                                py += (-dy * inv_d) * w2 - (dx * inv_d) * (w2 * 0.65);

                                let mut local_st = clamp(pull * 0.85 + ph2.abs() * 0.40 + w2.abs() * 0.04, 0.0, 1.0);

                                // 4. Subtle 3-strand helical strain right around the throat boundary
                                if dist < braid_cut {
                                    let nx = -ak.uy;
                                    let ny = ak.ux;
                                    let s_ax = -dx * ak.ux - dy * ak.uy;
                                    let n_eq = -dx * nx - dy * ny;
                                    let r_b = dist / (r_cloak * 1.15);
                                    let rb2 = r_b * r_b;
                                    let braid_env = 1.0 / (1.0 + rb2 + 0.5 * rb2 * rb2);
                                    let strand_parity = (n_eq / spacing) * (2.0 * PI / 3.0);
                                    let helix = fast_sin(s_ax * (PI / r_cloak) * 1.4 * ak.handedness + ak.braid_phase + strand_parity);
                                    let amp = r_cloak * 0.14 * ak.braid_gain * braid_env;
                                    px += nx * helix * amp;
                                    py += ny * helix * amp;
                                    local_st = local_st.max(clamp(braid_env * ak.braid_gain * 0.85, 0.0, 1.0));
                                }

                                self.grid[idx].x += px;
                                self.grid[idx].y += py;
                                if local_st > self.grid[idx].strain {
                                    self.grid[idx].strain = local_st;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Pass 3: Smooth domain-locked displacement saturation + outer boundary framing + anti-fold relaxation
        // Locks the manifold frame strictly to [0, width] x [0, height] with zero rightward/downward drift
        let pad_x = (dx_g * 2.0).min(w_span * 0.25);
        let pad_y = (dy_g * 2.0).min(h_span * 0.25);
        let max_disp = 28.0f32;
        for c in 0..cols {
            for r in 0..rows {
                let idx = c * rows + r;
                if idx >= grid_len { continue; }
                let ox = self.grid[idx].ox;
                let oy = self.grid[idx].oy;
                let mut dx_tot = self.grid[idx].x;
                let mut dy_tot = self.grid[idx].y;
                let mag = (dx_tot * dx_tot + dy_tot * dy_tot).sqrt();
                if mag > 0.001 {
                    let s = max_disp / (max_disp + mag);
                    dx_tot *= s;
                    dy_tot *= s;
                }
                let ex_t = clamp(ox.min(w_span - ox) / pad_x.max(1.0), 0.0, 1.0);
                let ey_t = clamp(oy.min(h_span - oy) / pad_y.max(1.0), 0.0, 1.0);
                let tx_edge = ex_t * ex_t * (3.0 - 2.0 * ex_t);
                let ty_edge = ey_t * ey_t * (3.0 - 2.0 * ey_t);

                self.grid[idx].x = clamp(ox + dx_tot * tx_edge, 0.0, w_span);
                self.grid[idx].y = clamp(oy + dy_tot * ty_edge, 0.0, h_span);
            }
        }

        // Two fast anti-folding relaxation sweeps guarantee a bijective mesh (zero inverted/crossed cells)
        let min_cx = dx_g * 0.22;
        let min_cy = dy_g * 0.22;
        for _ in 0..2 {
            let mut any_fold = false;
            for c in 1..(cols - 1) {
                for r in 1..(rows - 1) {
                    let idx = c * rows + r;
                    if idx >= grid_len { continue; }
                    let xl = self.grid[(c - 1) * rows + r].x;
                    let xr = self.grid[(c + 1) * rows + r].x;
                    let yu = self.grid[c * rows + (r - 1)].y;
                    let yd = self.grid[c * rows + (r + 1)].y;
                    let x_cur = self.grid[idx].x;
                    let y_cur = self.grid[idx].y;
                    if (x_cur - xl) < min_cx || (xr - x_cur) < min_cx {
                        self.grid[idx].x = lerp(x_cur, 0.5 * (xl + xr), 0.55);
                        any_fold = true;
                    }
                    if (y_cur - yu) < min_cy || (yd - y_cur) < min_cy {
                        self.grid[idx].y = lerp(y_cur, 0.5 * (yu + yd), 0.55);
                        any_fold = true;
                    }
                }
            }
            if !any_fold { break; }
        }

        for idx in 0..grid_len {
            let off = idx * 8;
            self.render_grid[off] = self.grid[idx].x;
            self.render_grid[off + 1] = self.grid[idx].y;
            self.render_grid[off + 2] = self.grid[idx].ex;
            self.render_grid[off + 3] = self.grid[idx].ey;
            self.render_grid[off + 4] = self.grid[idx].bz;
            self.render_grid[off + 5] = self.grid[idx].strain;
            self.render_grid[off + 6] = self.grid[idx].u;
            self.render_grid[off + 7] = self.grid[idx].slope;
        }
    }

    fn pack_outputs(&mut self, v_c2: f32) {
        let count = self.particle_count.min(MAX_PARTICLES);
        let grid_len = (self.grid_cols * self.grid_rows).min(MAX_GRID_NODES);
        let c2 = (self.c * self.c).max(0.01);
        let warp_to_manifold = self.render_mode == 1;
        let w_span = self.width.max(1.0);
        let h_span = self.height.max(1.0);

        let mut trapped_counts = [0i32; MAX_KNOTS];
        let mut total_e = 0.0f32;
        let mut net_q = 0.0f32;

        for i in 0..count {
            let p = self.particles[i];
            if p.trapped_in >= 0 && (p.trapped_in as usize) < MAX_KNOTS {
                trapped_counts[p.trapped_in as usize] += 1;
            }
            let (pr, pg, pb) = if self.em_color_mode {
                let q_abs = p.charge.abs().clamp(0.0, 1.0);
                if p.twist_thirds > 0 {
                    (lerp(p.r, 0.96, 0.45 + 0.45 * q_abs), lerp(p.g, 0.58, 0.45 + 0.45 * q_abs), lerp(p.b, 0.20, 0.45 + 0.45 * q_abs))
                } else if p.twist_thirds < 0 {
                    (lerp(p.r, 0.34, 0.45 + 0.45 * q_abs), lerp(p.g, 0.78, 0.45 + 0.45 * q_abs), lerp(p.b, 0.85, 0.45 + 0.45 * q_abs))
                } else {
                    (p.r * 0.78, p.g * 0.78, p.b * 0.78)
                }
            } else {
                (p.r, p.g, p.b)
            };

            let (rx, ry, rpx, rpy) = if warp_to_manifold {
                let (wx, wy) = self.sample_manifold_pos(p.x, p.y);
                let wrapped = (p.x - p.prev_x).abs() > w_span * 0.5 || (p.y - p.prev_y).abs() > h_span * 0.5;
                if wrapped {
                    (wx, wy, wx, wy)
                } else {
                    let (wpx, wpy) = self.sample_manifold_pos(p.prev_x, p.prev_y);
                    (wx, wy, wpx, wpy)
                }
            } else {
                (p.x, p.y, p.prev_x, p.prev_y)
            };

            let off = i * 7;
            self.render_particles[off] = rx;
            self.render_particles[off + 1] = ry;
            self.render_particles[off + 2] = pr;
            self.render_particles[off + 3] = pg;
            self.render_particles[off + 4] = pb;
            self.render_particles[off + 5] = 1.0;
            self.render_particles[off + 6] = p.size;

            let toff = i * 8;
            self.render_trails[toff] = rpx;
            self.render_trails[toff + 1] = rpy;
            self.render_trails[toff + 2] = rx;
            self.render_trails[toff + 3] = ry;
            self.render_trails[toff + 4] = pr;
            self.render_trails[toff + 5] = pg;
            self.render_trails[toff + 6] = pb;
            self.render_trails[toff + 7] = 0.35;

            let v2 = p.vx * p.vx + p.vy * p.vy;
            let gamma = 1.0 / (1.0 - (v2 / v_c2).min(0.97)).sqrt();
            total_e += gamma * p.freq_mult * self.base_mass * c2;
            net_q += p.charge;
        }

        let mut em_energy = 0.0f32;
        for g in self.grid[..grid_len].iter() {
            em_energy += 0.5 * (g.ex * g.ex + g.ey * g.ey + g.bz * g.bz);
        }

        if self.render_mode != 1 && self.render_mode != 0 {
            for (idx, g) in self.grid[..grid_len].iter().enumerate() {
                let off = idx * 8;
                self.render_grid[off] = g.ox;
                self.render_grid[off + 1] = g.oy;
                self.render_grid[off + 2] = g.ex;
                self.render_grid[off + 3] = g.ey;
                self.render_grid[off + 4] = g.bz;
                self.render_grid[off + 5] = 0.0;
                self.render_grid[off + 6] = g.u;
                self.render_grid[off + 7] = g.slope;
            }
        }

        let mut active_k = 0.0f32;
        for i in 0..MAX_KNOTS {
            let off = i * KNOT_STRIDE;
            if self.knots[i].active {
                let k = self.knots[i];
                let (kr, kg, kb) = if self.em_color_mode {
                    if k.charge > 0.3 {
                        (0.90, 0.65, 0.22)
                    } else if k.charge < -0.3 {
                        (0.37, 0.72, 0.75)
                    } else {
                        (0.75, 0.65, 0.48)
                    }
                } else {
                    (0.78, 0.66, 0.44)
                };
                self.render_knots[off] = k.x;
                self.render_knots[off + 1] = k.y;
                self.render_knots[off + 2] = k.radius;
                self.render_knots[off + 3] = k.charge;
                self.render_knots[off + 4] = 0.0;
                self.render_knots[off + 5] = kr;
                self.render_knots[off + 6] = kg;
                self.render_knots[off + 7] = kb;
                self.render_knots[off + 8] = k.folding_progress;
                self.render_knots[off + 9] = k.angular_vel;
                self.render_knots[off + 10] = trapped_counts[i] as f32;
                self.render_knots[off + 11] = 1.0;
                self.render_knots[off + 12] = k.axis_angle.cos();
                self.render_knots[off + 13] = k.axis_angle.sin();
                self.render_knots[off + 14] = k.cloak_radius;
                self.render_knots[off + 15] = k.braid_phase;

                active_k += 1.0;
                net_q += k.charge;
            } else {
                self.render_knots[off + 11] = 0.0;
            }
        }
        let atom_k = 0.0f32;
        let gluon_k = 0.0f32;
        let quark_k = 0.0f32;

        for i in 0..MAX_BONDS {
            let b = self.bonds[i];
            let off = i * 8;
            if b.active && self.knots[b.a].active && self.knots[b.b].active {
                let (ax_b, ay_b) = if warp_to_manifold {
                    self.sample_manifold_pos(self.knots[b.a].x, self.knots[b.a].y)
                } else {
                    (self.knots[b.a].x, self.knots[b.a].y)
                };
                let (bx_b, by_b) = if warp_to_manifold {
                    self.sample_manifold_pos(self.knots[b.b].x, self.knots[b.b].y)
                } else {
                    (self.knots[b.b].x, self.knots[b.b].y)
                };
                self.render_bonds[off] = ax_b;
                self.render_bonds[off + 1] = ay_b;
                self.render_bonds[off + 2] = bx_b;
                self.render_bonds[off + 3] = by_b;
                self.render_bonds[off + 4] = 0.88;
                self.render_bonds[off + 5] = 0.62;
                self.render_bonds[off + 6] = 0.25;
                self.render_bonds[off + 7] = clamp(0.45 + b.tension * 0.5, 0.35, 0.95);
            } else {
                self.render_bonds[off + 7] = 0.0;
            }
        }

        for i in 0..MAX_SHOCKWAVES {
            let sw = self.shockwaves[i];
            let off = i * 4;
            if sw.active && sw.kinetic_push > 0.0 {
                let (sx_w, sy_w) = if warp_to_manifold {
                    self.sample_manifold_pos(sw.x, sw.y)
                } else {
                    (sw.x, sw.y)
                };
                self.render_shockwaves[off] = sx_w;
                self.render_shockwaves[off + 1] = sy_w;
                self.render_shockwaves[off + 2] = sw.radius;
                self.render_shockwaves[off + 3] = (1.0 - sw.age / sw.max_age).max(0.0);
            } else {
                self.render_shockwaves[off + 3] = 0.0;
            }
        }

        self.metrics = [active_k, atom_k, gluon_k, quark_k, total_e, net_q, em_energy, self.planck_mass];
    }

    fn step(&mut self) {
        self.update_derived_constants();
        if self.attr_active {
            if self.render_mode == 1 {
                let (ux, uy) = self.unmap_manifold_pos(self.attr_raw_x, self.attr_raw_y);
                self.attr_x = ux;
                self.attr_y = uy;
            } else {
                self.attr_x = self.attr_raw_x;
                self.attr_y = self.attr_raw_y;
            }
        }
        let total_speed = clamp(self.sim_speed, 0.05, 10.0);
        let sub_steps = (total_speed / 2.0).ceil().clamp(1.0, 5.0) as usize;
        let sub_dt = total_speed / sub_steps as f32;
        let saved_speed = self.sim_speed;
        self.sim_speed = sub_dt;

        let g_sim = if self.gravity <= 0.0001 { 0.0 } else { self.gravity * 20.0 };
        let v_c = (self.c * 4.0).max(0.5);
        let v_c2 = v_c * v_c;

        for sub_idx in 0..sub_steps {
            let dt = 0.05 * sub_dt;
            self.time += dt;

            for sw in self.shockwaves.iter_mut() {
                if sw.active {
                    sw.radius += sw.speed * sub_dt;
                    sw.age += 1.0 * sub_dt;
                    if sw.age >= sw.max_age || sw.radius >= sw.max_radius {
                        sw.active = false;
                    }
                }
            }

            self.step_em_field(dt);
            self.step_knot_nucleation(g_sim, v_c, v_c2);
            self.step_knots_dynamics(g_sim, v_c);
            self.step_wave_sheet();
            self.step_pauli_degeneracy();
            self.step_particles(g_sim, v_c, v_c2);
            self.step_collisions();
            if sub_idx + 1 == sub_steps {
                if self.render_mode == 1 {
                    self.step_metric_grid(g_sim);
                }
                self.pack_outputs(v_c2);
            }
            self.rng_seed = self.rng_seed.wrapping_add(1);
        }
        self.sim_speed = saved_speed;
    }
}

// --- C ABI Exports for Zero-Dependency JS <-> WASM Interop ---

#[inline]
fn finite_or(v: f32, fallback: f32) -> f32 {
    if v.is_finite() { v } else { fallback }
}

#[no_mangle]
pub extern "C" fn sim_init(width: f32, height: f32) {
    let s = get_sim();
    s.width = finite_or(width, 540.0).max(60.0);
    s.height = finite_or(height, 380.0).max(60.0);
    s.init_particles();
}

#[no_mangle]
pub extern "C" fn sim_resize(width: f32, height: f32) {
    let s = get_sim();
    s.width = finite_or(width, s.width).max(60.0);
    s.height = finite_or(height, s.height).max(60.0);
    s.setup_grid();
    let v_c = (s.c * 4.0).max(0.5);
    if s.render_mode == 1 {
        let g_sim = if s.gravity <= 0.0001 { 0.0 } else { s.gravity * 20.0 };
        s.step_metric_grid(g_sim);
    }
    s.pack_outputs(v_c * v_c);
}

#[no_mangle]
pub extern "C" fn sim_set_params(
    medium_id: u32,
    pop_style: u32,
    particle_count: u32,
    frequency: f32,
    c: f32,
    h: f32,
    gravity: f32,
    drag: f32,
    sim_speed: f32,
    dark_energy: f32,
    orbital_boost: f32,
    gravity_range: f32,
    initial_spin: f32,
    center_bias: f32,
    em_coupling: f32,
) {
    let s = get_sim();
    s.medium_id = medium_id as u8;
    s.pop_style = pop_style as u8;
    s.particle_count = (particle_count as usize).min(MAX_PARTICLES);
    s.frequency = finite_or(frequency, s.frequency).max(0.0);
    s.c = finite_or(c, s.c).max(0.1);
    s.h = finite_or(h, s.h).max(0.01);
    s.gravity = finite_or(gravity, s.gravity).max(0.0);
    s.drag = clamp(finite_or(drag, s.drag), 0.0, 0.98);
    s.sim_speed = clamp(finite_or(sim_speed, s.sim_speed), 0.05, 10.0);
    s.dark_energy = finite_or(dark_energy, s.dark_energy).max(0.0);
    s.orbital_boost = finite_or(orbital_boost, s.orbital_boost).max(0.0);
    s.gravity_range = finite_or(gravity_range, s.gravity_range).max(0.0);
    s.initial_spin = clamp(finite_or(initial_spin, s.initial_spin), -25.0, 25.0);
    s.center_bias = clamp(finite_or(center_bias, s.center_bias), 0.0, 10.0);
    s.em_coupling = clamp(finite_or(em_coupling, s.em_coupling), 0.0, 10.0);
}

#[no_mangle]
pub extern "C" fn sim_set_toggles(
    individual_pull: u32,
    intra_collisions: u32,
    non_destructible: u32,
    permanent_clumps: u32,
    trapped_mode: u32,
) {
    let s = get_sim();
    s.individual_pull = individual_pull != 0;
    s.intra_collisions = intra_collisions != 0;
    s.non_destructible = non_destructible != 0;
    s.permanent_clumps = permanent_clumps != 0;
    s.trapped_mode = trapped_mode as u8;
}

#[no_mangle]
pub extern "C" fn sim_set_wave_params(sheet_tension: f32, pole_gate_deg: f32) {
    let s = get_sim();
    s.sheet_tension = clamp(sheet_tension, 0.04, 0.35);
    s.pole_gate_deg = clamp(pole_gate_deg, 5.0, 90.0);
}

#[no_mangle]
pub extern "C" fn sim_set_render_flags(em_color_mode: u32, render_mode: u32) {
    let s = get_sim();
    let next_em = em_color_mode != 0;
    let next_mode = (render_mode as u8).min(3);
    if s.em_color_mode != next_em || s.render_mode != next_mode {
        s.em_color_mode = next_em;
        s.render_mode = next_mode;
        let v_c = (s.c * 4.0).max(0.5);
        if s.render_mode == 1 {
            let g_sim = if s.gravity <= 0.0001 { 0.0 } else { s.gravity * 20.0 };
            s.step_metric_grid(g_sim);
        }
        s.pack_outputs(v_c * v_c);
    }
}

#[no_mangle]
pub extern "C" fn sim_set_attractor(active: u32, x: f32, y: f32, strength: f32) {
    let s = get_sim();
    s.attr_active = active != 0;
    s.attr_raw_x = x;
    s.attr_raw_y = y;
    if s.render_mode == 1 {
        let (ux, uy) = s.unmap_manifold_pos(x, y);
        s.attr_x = ux;
        s.attr_y = uy;
    } else {
        s.attr_x = x;
        s.attr_y = y;
    }
    s.attr_strength = strength;
}

#[no_mangle]
pub extern "C" fn sim_reseed() {
    get_sim().init_particles();
}

#[no_mangle]
pub extern "C" fn sim_explode() {
    get_sim().explode();
}

#[no_mangle]
pub extern "C" fn sim_supernova() {}

#[no_mangle]
pub extern "C" fn sim_step() {
    get_sim().step();
}

#[no_mangle]
pub extern "C" fn sim_particles_ptr() -> *const f32 {
    get_sim().render_particles.as_ptr()
}

#[no_mangle]
pub extern "C" fn sim_trails_ptr() -> *const f32 {
    get_sim().render_trails.as_ptr()
}

#[no_mangle]
pub extern "C" fn sim_grid_ptr() -> *const f32 {
    get_sim().render_grid.as_ptr()
}

#[no_mangle]
pub extern "C" fn sim_grid_dims() -> u32 {
    let s = get_sim();
    ((s.grid_cols as u32) << 16) | (s.grid_rows as u32)
}

#[no_mangle]
pub extern "C" fn sim_knots_ptr() -> *const f32 {
    get_sim().render_knots.as_ptr()
}

#[no_mangle]
pub extern "C" fn sim_bonds_ptr() -> *const f32 {
    get_sim().render_bonds.as_ptr()
}

#[no_mangle]
pub extern "C" fn sim_shockwaves_ptr() -> *const f32 {
    get_sim().render_shockwaves.as_ptr()
}

#[no_mangle]
pub extern "C" fn sim_metrics_ptr() -> *const f32 {
    get_sim().metrics.as_ptr()
}
