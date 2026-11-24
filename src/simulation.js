import { computePhysicsValues } from './math.js';

const POP_STYLE_MAP = {
  default: 0,
  orbital: 1,
  chaos: 2,
  spiral: 3,
  grid: 4,
  central: 5
};

const MEDIUM_MAP = { A: 0, B: 1, C: 2 };
const TRAPPED_MODE_MAP = { prob: 0, always: 1, off: 2 };
const RENDER_MODE_MAP = { physics: 0, grid: 1, fluid: 2, vector: 3 };

export class Simulation {
  constructor(canvas) {
    this.canvas = canvas;
    this.wasm = null;
    this.memory = null;
    this.isWasmReady = false;

    // Play & Physics parameters
    this.mediumId = 'A';
    this.frequency = 2.4;
    this.c = 3.0;
    this.h = 6.62607015;
    this.mass = 0.736;
    this.particleDensity = 3000;
    this.gravity = 6.6743e-11;
    this.clumpThreshold = 1.0;
    this.drag = 0.0;
    this.simSpeed = 1.0;
    this.darkEnergy = 0.0;
    this.orbitalBoost = 50.0;
    this.gravityRange = 70.0;
    this.initialAngularVelocity = 1.0;
    this.centerBias = 0.0;
    this.emCoupling = 1.0;
    this.sheetTension = 0.22;
    this.poleGateDeg = 30.0;
    this.populationStyle = 'grid';

    // Toggles
    this.individualPull = true;
    this.intraCollisions = true;
    this.waveTrails = true;
    this.nonDestructibleCores = true;
    this.permanentClumps = true;
    this.trappedDots = true;
    this.trappedDotsMode = 'prob';
    this.renderMode = 'grid';
    this.sectorSize = 70;

    // Camera Zoom & World Coordinates (zoomed in 1.85x by default)
    this.baseZoom = 1.85;
    this.cameraZoom = 1.85;

    // Mouse Attractor
    this.attractor = { x: 0, y: 0, active: false, radius: 100, strength: 5.0 };

    // State
    this.isPaused = false;
    this.width = canvas.width || 1000;
    this.height = canvas.height || 700;
    this.worldWidth = Math.max(320, Math.round(this.width / this.baseZoom));
    this.worldHeight = Math.max(240, Math.round(this.height / this.baseZoom));

    // Cached metrics from WASM
    this.metrics = {
      activeKnots: 0,
      totalEnergy: 0,
      netCharge: 0,
      emEnergy: 0,
      planckMass: 1
    };
  }

  screenToWorld(sx, sy) {
    return {
      x: (sx - this.width * 0.5) / this.cameraZoom + this.worldWidth * 0.5,
      y: (sy - this.height * 0.5) / this.cameraZoom + this.worldHeight * 0.5
    };
  }

  async initWasm() {
    if (this.isWasmReady) return true;
    try {
      let instance;
      if (WebAssembly.instantiateStreaming) {
        try {
          const res = await WebAssembly.instantiateStreaming(fetch('./physics.wasm'), {});
          instance = res.instance;
        } catch {
          const buf = await (await fetch('./physics.wasm')).arrayBuffer();
          const res = await WebAssembly.instantiate(buf, {});
          instance = res.instance;
        }
      } else {
        const buf = await (await fetch('./physics.wasm')).arrayBuffer();
        const res = await WebAssembly.instantiate(buf, {});
        instance = res.instance;
      }

      this.wasm = instance.exports;
      this.memory = this.wasm.memory;
      this.isWasmReady = true;

      this.syncParamsToWasm();
      this.wasm.sim_init(this.worldWidth, this.worldHeight);
      return true;
    } catch (err) {
      console.error('Failed to load Rust WASM physics engine:', err);
      return false;
    }
  }

  syncParamsToWasm() {
    if (!this.isWasmReady) return;
    const gScaled = this.gravity * 1e11;
    const phys = computePhysicsValues(this.mediumId, this.frequency, this.c, this.h, gScaled);
    this.mass = phys.massOrDensity;
    this.clumpThreshold = phys.planckMass;

    this.wasm.sim_set_params(
      MEDIUM_MAP[this.mediumId] ?? 0,
      POP_STYLE_MAP[this.populationStyle] ?? 4,
      this.particleDensity | 0,
      this.frequency,
      this.c,
      this.h,
      gScaled,
      this.drag,
      this.sim_speed ?? this.simSpeed,
      this.darkEnergy,
      this.orbitalBoost,
      this.gravityRange / 1.4,
      this.initialAngularVelocity,
      this.centerBias,
      this.emCoupling
    );

    if (this.wasm.sim_set_wave_params) {
      this.wasm.sim_set_wave_params(this.sheetTension, this.poleGateDeg);
    }
    if (this.wasm.sim_set_render_flags) {
      this.wasm.sim_set_render_flags(
        this.renderMode === 'vector' ? 1 : 0,
        RENDER_MODE_MAP[this.renderMode] ?? 1
      );
    }

    this.wasm.sim_set_toggles(
      this.individualPull ? 1 : 0,
      this.intraCollisions ? 1 : 0,
      this.nonDestructibleCores ? 1 : 0,
      this.permanentClumps ? 1 : 0,
      TRAPPED_MODE_MAP[this.trappedDotsMode] ?? 0
    );

    this.wasm.sim_set_attractor(
      this.attractor.active ? 1 : 0,
      this.attractor.x,
      this.attractor.y,
      this.attractor.strength
    );
  }

  init() {
    if (this.isWasmReady) {
      this.syncParamsToWasm();
      this.wasm.sim_reseed();
    }
  }

  setupParticles() {
    this.init();
  }

  updateClumpThreshold() {
    const gScaled = this.gravity * 1e11;
    const phys = computePhysicsValues(this.mediumId, this.frequency, this.c, this.h, gScaled);
    this.clumpThreshold = phys.planckMass;
  }

  resize(width, height) {
    this.width = width;
    this.height = height;
    this.worldWidth = Math.max(320, Math.round(width / this.baseZoom));
    this.worldHeight = Math.max(240, Math.round(height / this.baseZoom));
    this.canvas.width = width;
    this.canvas.height = height;
    if (this.isWasmReady) {
      this.wasm.sim_resize(this.worldWidth, this.worldHeight);
    }
  }

  explode() {
    if (this.isWasmReady) {
      this.wasm.sim_explode();
    }
  }

  tick() {
    if (!this.isWasmReady || this.isPaused) return;
    this.syncParamsToWasm();
    this.wasm.sim_step();

    // Read metrics from WASM
    const mPtr = this.wasm.sim_metrics_ptr();
    const mView = new Float32Array(this.memory.buffer, mPtr, 8);
    this.metrics.activeKnots = mView[0] | 0;
    this.metrics.totalEnergy = mView[4];
    this.metrics.netCharge = mView[5];
    this.metrics.emEnergy = mView[6];
    this.metrics.planckMass = mView[7];
  }

  getWasmViews() {
    if (!this.isWasmReady) return null;
    const buf = this.memory.buffer;
    const dims = this.wasm.sim_grid_dims();
    const gridCols = (dims >>> 16) & 0xffff;
    const gridRows = dims & 0xffff;
    const count = Math.min(this.particleDensity, 50000);

    return {
      count,
      gridCols,
      gridRows,
      gridSpacing: 10.0,
      particles: new Float32Array(buf, this.wasm.sim_particles_ptr(), count * 7),
      trails: new Float32Array(buf, this.wasm.sim_trails_ptr(), count * 8),
      grid: new Float32Array(buf, this.wasm.sim_grid_ptr(), gridCols * gridRows * 8),
      knots: new Float32Array(buf, this.wasm.sim_knots_ptr(), 128 * 16),
      bonds: new Float32Array(buf, this.wasm.sim_bonds_ptr(), 128 * 8),
      shockwaves: new Float32Array(buf, this.wasm.sim_shockwaves_ptr(), 32 * 4)
    };
  }
}
