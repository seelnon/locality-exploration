// Pure WebGL2/WebGL1 Hardware-Accelerated Renderer reading directly from Rust WASM memory

export class WebGLRenderer {
  constructor(canvas) {
    this.canvas = canvas;
    this.gl = null;
    this.isWebGL2 = false;
    this.isReady = false;

    this.particleProgram = null;
    this.lineProgram = null;
    this.waveProgram = null;

    this.particleAttribs = {};
    this.lineAttribs = {};
    this.waveAttribs = {};

    this.particleBuffer = null;
    this.lineBuffer = null;
    this.waveQuadBuffer = null;
    this.waveTex = null;
    this.wavePixels = null;
    this.waveCols = 0;
    this.waveRows = 0;

    this.lineData = new Float32Array(90000 * 6);
  }

  async init() {
    const glOptions = {
      alpha: false,
      antialias: true,
      depth: false,
      stencil: false,
      preserveDrawingBuffer: false,
      powerPreference: 'high-performance'
    };

    let gl = this.canvas.getContext('webgl2', glOptions);
    if (gl) {
      this.isWebGL2 = true;
    } else {
      gl = this.canvas.getContext('webgl', glOptions) || this.canvas.getContext('experimental-webgl', glOptions);
      this.isWebGL2 = false;
    }

    if (!gl) {
      throw new Error('WebGL is not supported by your browser.');
    }

    this.gl = gl;
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
    gl.disable(gl.DEPTH_TEST);

    this.initParticleProgram();
    this.initLineProgram();
    this.initWaveProgram();

    this.particleBuffer = gl.createBuffer();
    this.lineBuffer = gl.createBuffer();
    this.waveQuadBuffer = gl.createBuffer();
    this.waveTex = gl.createTexture();

    gl.bindTexture(gl.TEXTURE_2D, this.waveTex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

    this.isReady = true;
  }

  createShader(type, source) {
    const gl = this.gl;
    const shader = gl.createShader(type);
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      const info = gl.getShaderInfoLog(shader);
      gl.deleteShader(shader);
      throw new Error(`WebGL Shader Error: ${info}`);
    }
    return shader;
  }

  createProgram(vsSource, fsSource) {
    const gl = this.gl;
    const vs = this.createShader(gl.VERTEX_SHADER, vsSource);
    const fs = this.createShader(gl.FRAGMENT_SHADER, fsSource);
    const program = gl.createProgram();
    gl.attachShader(program, vs);
    gl.attachShader(program, fs);
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      const info = gl.getProgramInfoLog(program);
      gl.deleteProgram(program);
      throw new Error(`WebGL Link Error: ${info}`);
    }
    return program;
  }

  initParticleProgram() {
    const gl = this.gl;
    const vs = `
      precision highp float;
      attribute vec2 a_position;
      attribute vec4 a_color;
      attribute float a_size;
      uniform vec2 u_resolution;
      uniform vec2 u_world_center;
      uniform float u_zoom;
      uniform float u_alpha_scale;
      uniform float u_dpr;
      varying vec4 v_color;

      void main() {
        vec2 screenPos = (a_position - u_world_center) * u_zoom + u_resolution * 0.5;
        vec2 clipSpace = (screenPos / u_resolution) * 2.0 - 1.0;
        gl_Position = vec4(clipSpace.x, -clipSpace.y, 0.0, 1.0);
        gl_PointSize = max(2.0, a_size * u_zoom * u_dpr * 1.35);
        v_color = vec4(a_color.rgb, a_color.a * u_alpha_scale);
      }
    `;
    const fs = `
      precision mediump float;
      varying vec4 v_color;
      void main() {
        vec2 coord = gl_PointCoord - vec2(0.5);
        float dist = length(coord);
        if (dist > 0.5) discard;
        float alpha = smoothstep(0.5, 0.32, dist);
        gl_FragColor = vec4(v_color.rgb, v_color.a * alpha);
      }
    `;
    this.particleProgram = this.createProgram(vs, fs);
    this.particleAttribs = {
      position: gl.getAttribLocation(this.particleProgram, 'a_position'),
      color: gl.getAttribLocation(this.particleProgram, 'a_color'),
      size: gl.getAttribLocation(this.particleProgram, 'a_size'),
      resolution: gl.getUniformLocation(this.particleProgram, 'u_resolution'),
      worldCenter: gl.getUniformLocation(this.particleProgram, 'u_world_center'),
      zoom: gl.getUniformLocation(this.particleProgram, 'u_zoom'),
      alphaScale: gl.getUniformLocation(this.particleProgram, 'u_alpha_scale'),
      dpr: gl.getUniformLocation(this.particleProgram, 'u_dpr')
    };
  }

  initLineProgram() {
    const gl = this.gl;
    const vs = `
      precision highp float;
      attribute vec2 a_position;
      attribute vec4 a_color;
      uniform vec2 u_resolution;
      uniform vec2 u_world_center;
      uniform float u_zoom;
      varying vec4 v_color;
      void main() {
        vec2 screenPos = (a_position - u_world_center) * u_zoom + u_resolution * 0.5;
        vec2 clipSpace = (screenPos / u_resolution) * 2.0 - 1.0;
        gl_Position = vec4(clipSpace.x, -clipSpace.y, 0.0, 1.0);
        v_color = a_color;
      }
    `;
    const fs = `
      precision mediump float;
      varying vec4 v_color;
      void main() {
        gl_FragColor = v_color;
      }
    `;
    this.lineProgram = this.createProgram(vs, fs);
    this.lineAttribs = {
      position: gl.getAttribLocation(this.lineProgram, 'a_position'),
      color: gl.getAttribLocation(this.lineProgram, 'a_color'),
      resolution: gl.getUniformLocation(this.lineProgram, 'u_resolution'),
      worldCenter: gl.getUniformLocation(this.lineProgram, 'u_world_center'),
      zoom: gl.getUniformLocation(this.lineProgram, 'u_zoom')
    };
  }


  initWaveProgram() {
    const gl = this.gl;
    const vs = `
      precision highp float;
      attribute vec2 a_position;
      attribute vec2 a_uv;
      uniform vec2 u_resolution;
      uniform vec2 u_world_center;
      uniform float u_zoom;
      varying vec2 v_uv;
      void main() {
        vec2 screenPos = (a_position - u_world_center) * u_zoom + u_resolution * 0.5;
        vec2 clipSpace = (screenPos / u_resolution) * 2.0 - 1.0;
        gl_Position = vec4(clipSpace.x, -clipSpace.y, 0.0, 1.0);
        v_uv = a_uv;
      }
    `;
    const fs = `
      precision mediump float;
      uniform sampler2D u_wave_tex;
      uniform float u_intensity;
      varying vec2 v_uv;
      void main() {
        vec4 w = texture2D(u_wave_tex, v_uv);
        float dent = w.r;
        float crest = w.g;
        float slope = (w.b - 0.5) * 2.0;
        float flux = w.a;

        // Brutalist Dark Coffee paper base (#1c1814) -> Elden Altus Gold dents & illuminated wave crests
        vec3 baseCol = vec3(0.110, 0.094, 0.078);
        vec3 dentCol = vec3(0.46, 0.34, 0.14);
        vec3 crestCol = vec3(0.68, 0.55, 0.30);

        vec3 col = mix(baseCol, dentCol, dent * 0.85 * u_intensity);
        col = mix(col, crestCol, crest * 0.48 * u_intensity);
        col += vec3(0.25, 0.21, 0.12) * slope * u_intensity;
        gl_FragColor = vec4(clamp(col, 0.0, 1.0), 1.0);
      }
    `;
    this.waveProgram = this.createProgram(vs, fs);
    this.waveAttribs = {
      position: gl.getAttribLocation(this.waveProgram, 'a_position'),
      uv: gl.getAttribLocation(this.waveProgram, 'a_uv'),
      resolution: gl.getUniformLocation(this.waveProgram, 'u_resolution'),
      worldCenter: gl.getUniformLocation(this.waveProgram, 'u_world_center'),
      zoom: gl.getUniformLocation(this.waveProgram, 'u_zoom'),
      waveTex: gl.getUniformLocation(this.waveProgram, 'u_wave_tex'),
      intensity: gl.getUniformLocation(this.waveProgram, 'u_intensity')
    };
  }

  syncParticlesToGPU() {}

  resize(width, height) {
    if (!this.gl) return;
    this.canvas.width = width;
    this.canvas.height = height;
    this.gl.viewport(0, 0, width, height);
  }

  drawFrame(sim) {
    if (!this.isReady) return;
    const views = sim.getWasmViews();
    if (!views) return;

    const gl = this.gl;
    if (this.canvas.width !== sim.width || this.canvas.height !== sim.height) {
      this.resize(sim.width, sim.height);
    }

    const width = sim.width;
    const height = sim.height;

    // Deepened coffee workspace (#1c1814 -> 0.110, 0.094, 0.078)
    gl.viewport(0, 0, width, height);
    gl.clearColor(0.110, 0.094, 0.078, 1.0);
    gl.clear(gl.COLOR_BUFFER_BIT);

    // Only render 2D continuous wave-sheet manifold in FLUID mode
    if (sim.renderMode === 'fluid') {
      this.renderWaveSheet(sim, views, width, height);
    }
    this.renderLines(sim, views, width, height);
    this.renderParticles(sim, views, width, height);
  }

  renderWaveSheet(sim, views, width, height) {
    const gl = this.gl;
    const cols = views.gridCols;
    const rows = views.gridRows;
    if (cols < 2 || rows < 2) return;

    const total = cols * rows;
    if (!this.wavePixels || this.wavePixels.length !== total * 4) {
      this.wavePixels = new Uint8Array(total * 4);
    }

    const gBuf = views.grid;
    const px = this.wavePixels;
    // WebGL texImage2D expects row-major (rows height, cols width): pixel (c, r) at (r * cols + c)
    for (let r = 0; r < rows; r++) {
      for (let c = 0; c < cols; c++) {
        const gOff = (c * rows + r) * 8;
        const pOff = (r * cols + c) * 4;
        const u = gBuf[gOff + 6];
        const slope = gBuf[gOff + 7];

        const dent = Math.max(0, Math.min(1, -u / 4.8));
        const crest = Math.max(0, Math.min(1, u / 3.2));
        const slopeByte = Math.max(0, Math.min(255, Math.round((slope * 0.38 + 0.5) * 255)));
        const fluxByte = 0;

        px[pOff] = Math.round(dent * 255);
        px[pOff + 1] = Math.round(crest * 255);
        px[pOff + 2] = slopeByte;
        px[pOff + 3] = fluxByte;
      }
    }

    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, this.waveTex);
    gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, cols, rows, 0, gl.RGBA, gl.UNSIGNED_BYTE, px);

    const worldW = sim.worldWidth || width;
    const worldH = sim.worldHeight || height;
    const quad = new Float32Array([
      0, 0, 0, 0,
      worldW, 0, 1, 0,
      0, worldH, 0, 1,
      worldW, worldH, 1, 1
    ]);

    const intensity = sim.renderMode === 'fluid' ? 1.15 : 0.0;

    gl.useProgram(this.waveProgram);
    gl.uniform2f(this.waveAttribs.resolution, width, height);
    gl.uniform2f(this.waveAttribs.worldCenter, worldW * 0.5, worldH * 0.5);
    gl.uniform1f(this.waveAttribs.zoom, sim.cameraZoom || 1.85);
    gl.uniform1i(this.waveAttribs.waveTex, 0);
    gl.uniform1f(this.waveAttribs.intensity, intensity);

    gl.bindBuffer(gl.ARRAY_BUFFER, this.waveQuadBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, quad, gl.DYNAMIC_DRAW);
    gl.enableVertexAttribArray(this.waveAttribs.position);
    gl.vertexAttribPointer(this.waveAttribs.position, 2, gl.FLOAT, false, 16, 0);
    gl.enableVertexAttribArray(this.waveAttribs.uv);
    gl.vertexAttribPointer(this.waveAttribs.uv, 2, gl.FLOAT, false, 16, 8);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
  }

  renderLines(sim, views, width, height) {
    const gl = this.gl;
    let lineIdx = 0;

    const ensureCap = (needed) => {
      if (lineIdx + needed > this.lineData.length) {
        const next = new Float32Array(Math.max(this.lineData.length * 2, lineIdx + needed + 16384));
        next.set(this.lineData.subarray(0, lineIdx));
        this.lineData = next;
      }
    };

    const pushLine = (x1, y1, x2, y2, r, g, b, a) => {
      this.lineData[lineIdx++] = x1;
      this.lineData[lineIdx++] = y1;
      this.lineData[lineIdx++] = r;
      this.lineData[lineIdx++] = g;
      this.lineData[lineIdx++] = b;
      this.lineData[lineIdx++] = a;

      this.lineData[lineIdx++] = x2;
      this.lineData[lineIdx++] = y2;
      this.lineData[lineIdx++] = r;
      this.lineData[lineIdx++] = g;
      this.lineData[lineIdx++] = b;
      this.lineData[lineIdx++] = a;
    };

    // 1. Background Sector Lines (ONLY in METRIC_GRID mode; dark empty background for PARTICLES / FLUID / EM_FIELD)
    const worldW = sim.worldWidth || width;
    const worldH = sim.worldHeight || height;
    if (sim.renderMode === 'grid') {
      const spacing = sim.sectorSize || 70;
      const secAlpha = 0.28;
      ensureCap((Math.ceil(worldW / spacing) + Math.ceil(worldH / spacing) + 4) * 12);
      for (let x = 0; x <= worldW; x += spacing) {
        pushLine(x, 0, x, worldH, 0.23, 0.19, 0.15, secAlpha);
      }
      for (let y = 0; y <= worldH; y += spacing) {
        pushLine(0, y, worldW, y, 0.23, 0.19, 0.15, secAlpha);
      }
    }

    // 2. Spacetime Metric Distortion Grid (with embedded Bilson-Thompson 3-strand braid strain + wave crests)
    const cols = views.gridCols;
    const rows = views.gridRows;
    const gBuf = views.grid;

    if (sim.renderMode === 'grid' && cols > 1 && rows > 1) {
      ensureCap(cols * rows * 24);
      for (let c = 0; c < cols; c++) {
        for (let r = 0; r < rows - 1; r++) {
          const i1 = (c * rows + r) * 8;
          const i2 = (c * rows + r + 1) * 8;
          const strain = Math.max(gBuf[i1 + 5], gBuf[i2 + 5]);
          const cr = 0.30 + strain * 0.62;
          const cg = 0.25 + strain * 0.42;
          const cb = 0.18 + strain * 0.04;
          pushLine(gBuf[i1], gBuf[i1 + 1], gBuf[i2], gBuf[i2 + 1], cr, cg, cb, 0.46 + strain * 0.36);
        }
      }
      for (let r = 0; r < rows; r++) {
        for (let c = 0; c < cols - 1; c++) {
          const i1 = (c * rows + r) * 8;
          const i2 = ((c + 1) * rows + r) * 8;
          const strain = Math.max(gBuf[i1 + 5], gBuf[i2 + 5]);
          const cr = 0.30 + strain * 0.62;
          const cg = 0.25 + strain * 0.42;
          const cb = 0.18 + strain * 0.04;
          pushLine(gBuf[i1], gBuf[i1 + 1], gBuf[i2], gBuf[i2 + 1], cr, cg, cb, 0.46 + strain * 0.36);
        }
      }
    }

    // 3. Electromagnetic Vector Field View (EM_FIELD [E, B] mode)
    if (sim.renderMode === 'vector' && cols > 1 && rows > 1) {
      const step = 3;
      const eThresh = 0.015;
      const bThresh = 0.03;
      ensureCap(cols * rows * 24);

      for (let c = 0; c < cols; c += step) {
        for (let r = 0; r < rows; r += step) {
          const i = (c * rows + r) * 8;
          const x = gBuf[i];
          const y = gBuf[i + 1];
          const ex = gBuf[i + 2];
          const ey = gBuf[i + 3];
          const bz = gBuf[i + 4];
          const mag = Math.sqrt(ex * ex + ey * ey);
          const absBz = Math.abs(bz);

          // Magnetic Bz circulation indicator
          if (absBz > bThresh) {
            const bSize = Math.min(4.0, 1.6 + absBz * 2.2);
            const br = bz > 0 ? 0.37 : 0.85;
            const bg = bz > 0 ? 0.68 : 0.36;
            const bb = bz > 0 ? 0.70 : 0.22;
            const ba = Math.min(0.75, 0.25 + absBz * 0.4);
            pushLine(x - bSize, y, x + bSize, y, br, bg, bb, ba);
            pushLine(x, y - bSize, x, y + bSize, br, bg, bb, ba);
          }

          // Electric field E vector arrow (Burnished Elden Gold, world-space lengths scaled by u_zoom)
          if (mag > eThresh) {
            const len = Math.min(13, 3 + mag * 2.8);
            const nx = ex / mag;
            const ny = ey / mag;
            const endX = x + nx * len;
            const endY = y + ny * len;
            const alpha = 0.84;
            pushLine(x, y, endX, endY, 0.85, 0.70, 0.24, alpha);
            pushLine(endX, endY, endX - nx * 2.2 - ny * 1.2, endY - ny * 2.2 + nx * 1.2, 0.96, 0.83, 0.36, alpha);
            pushLine(endX, endY, endX - nx * 2.2 + ny * 1.2, endY - ny * 2.2 - nx * 1.2, 0.96, 0.83, 0.36, alpha);
          } else if (absBz <= bThresh) {
            pushLine(x - 0.9, y, x + 0.9, y, 0.35, 0.29, 0.21, 0.24);
          }
        }
      }
    }

    // 4. Metric Flux Bonds (tiled across borders without full-screen streaks)
    const bBuf = views.bonds;
    ensureCap(128 * 24);
    for (let i = 0; i < 128; i++) {
      const off = i * 8;
      const a = bBuf[off + 7];
      if (a > 0.01) {
        const x1 = bBuf[off];
        const y1 = bBuf[off + 1];
        const x2 = bBuf[off + 2];
        const y2 = bBuf[off + 3];
        let dx = x2 - x1;
        let dy = y2 - y1;
        let wrapped = false;
        if (dx > worldW * 0.5) { dx -= worldW; wrapped = true; }
        else if (dx < -worldW * 0.5) { dx += worldW; wrapped = true; }
        if (dy > worldH * 0.5) { dy -= worldH; wrapped = true; }
        else if (dy < -worldH * 0.5) { dy += worldH; wrapped = true; }
        const br = bBuf[off + 4];
        const bg = bBuf[off + 5];
        const bb = bBuf[off + 6];
        if (!wrapped) {
          pushLine(x1, y1, x2, y2, br, bg, bb, a);
        } else {
          pushLine(x1, y1, x1 + dx, y1 + dy, br, bg, bb, a);
          pushLine(x2 - dx, y2 - dy, x2, y2, br, bg, bb, a);
        }
      }
    }

    // 5. Particle Trails
    if (sim.waveTrails) {
      const tBuf = views.trails;
      const count = views.count;
      ensureCap(count * 12);
      const trailAlpha = sim.renderMode === 'physics' ? 0.38 : 0.22;
      for (let i = 0; i < count; i++) {
        const off = i * 8;
        const x1 = tBuf[off];
        const y1 = tBuf[off + 1];
        const x2 = tBuf[off + 2];
        const y2 = tBuf[off + 3];
        const dx = x2 - x1;
        const dy = y2 - y1;
        if (dx * dx + dy * dy < 400) {
          pushLine(x1, y1, x2, y2, tBuf[off + 4], tBuf[off + 5], tBuf[off + 6], trailAlpha);
        }
      }
    }

    // 6. Shockwaves (tiled across edges)
    const swBuf = views.shockwaves;
    ensureCap(32 * 4 * 32 * 12);
    for (let i = 0; i < 32; i++) {
      const off = i * 4;
      const alpha = swBuf[off + 3];
      if (alpha > 0.01) {
        const sx = swBuf[off];
        const sy = swBuf[off + 1];
        const rad = swBuf[off + 2];
        const segs = 32;
        const step = (Math.PI * 2) / segs;
        const oxList = [0];
        if (sx - rad < 0) oxList.push(worldW);
        if (sx + rad > worldW) oxList.push(-worldW);
        const oyList = [0];
        if (sy - rad < 0) oyList.push(worldH);
        if (sy + rad > worldH) oyList.push(-worldH);

        for (let oi = 0; oi < oxList.length; oi++) {
          for (let oj = 0; oj < oyList.length; oj++) {
            const cx = sx + oxList[oi];
            const cy = sy + oyList[oj];
            for (let s = 0; s < segs; s++) {
              const a1 = s * step;
              const a2 = (s + 1) * step;
              pushLine(
                cx + Math.cos(a1) * rad, cy + Math.sin(a1) * rad,
                cx + Math.cos(a2) * rad, cy + Math.sin(a2) * rad,
                0.85, 0.55, 0.20, alpha * 0.55
              );
            }
          }
        }
      }
    }

    if (lineIdx > 0) {
      gl.useProgram(this.lineProgram);
      gl.uniform2f(this.lineAttribs.resolution, width, height);
      gl.uniform2f(this.lineAttribs.worldCenter, worldW * 0.5, worldH * 0.5);
      gl.uniform1f(this.lineAttribs.zoom, sim.cameraZoom || 1.85);
      gl.bindBuffer(gl.ARRAY_BUFFER, this.lineBuffer);
      gl.bufferData(gl.ARRAY_BUFFER, this.lineData.subarray(0, lineIdx), gl.DYNAMIC_DRAW);

      const stride = 6 * 4;
      gl.enableVertexAttribArray(this.lineAttribs.position);
      gl.vertexAttribPointer(this.lineAttribs.position, 2, gl.FLOAT, false, stride, 0);
      gl.enableVertexAttribArray(this.lineAttribs.color);
      gl.vertexAttribPointer(this.lineAttribs.color, 4, gl.FLOAT, false, stride, 2 * 4);
      gl.drawArrays(gl.LINES, 0, lineIdx / 6);
    }
  }

  renderParticles(sim, views, width, height) {
    const gl = this.gl;
    const count = views.count;
    if (count === 0) return;

    const worldW = sim.worldWidth || width;
    const worldH = sim.worldHeight || height;
    const alphaScale = sim.renderMode === 'physics' ? 1.0
      : (sim.renderMode === 'grid' ? 0.80
      : (sim.renderMode === 'fluid' ? 0.72 : 0.56));
    const dpr = window.devicePixelRatio || 1.0;

    gl.useProgram(this.particleProgram);
    gl.uniform2f(this.particleAttribs.resolution, width, height);
    gl.uniform2f(this.particleAttribs.worldCenter, worldW * 0.5, worldH * 0.5);
    gl.uniform1f(this.particleAttribs.zoom, sim.cameraZoom || 1.85);
    gl.uniform1f(this.particleAttribs.alphaScale, alphaScale);
    gl.uniform1f(this.particleAttribs.dpr, dpr);

    // Zero-copy upload directly from WASM Float32Array slice!
    gl.bindBuffer(gl.ARRAY_BUFFER, this.particleBuffer);
    gl.bufferData(gl.ARRAY_BUFFER, views.particles, gl.DYNAMIC_DRAW);

    const stride = 7 * 4;
    gl.enableVertexAttribArray(this.particleAttribs.position);
    gl.vertexAttribPointer(this.particleAttribs.position, 2, gl.FLOAT, false, stride, 0);
    gl.enableVertexAttribArray(this.particleAttribs.color);
    gl.vertexAttribPointer(this.particleAttribs.color, 4, gl.FLOAT, false, stride, 2 * 4);
    gl.enableVertexAttribArray(this.particleAttribs.size);
    gl.vertexAttribPointer(this.particleAttribs.size, 1, gl.FLOAT, false, stride, 6 * 4);

    gl.drawArrays(gl.POINTS, 0, count);
  }
}
