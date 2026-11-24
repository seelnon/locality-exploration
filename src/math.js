export const MEDIUMS = {
  A: {
    id: 'A',
    name: 'Metric A: Cosmic Filament (λLc²)',
    formula: 'ν = λ L c² / h',
    leftFormula: 'E = λ L c²',
    rightFormula: 'E = hν',
    solvedFormula: 'λ = (ν · h) / (L · c²)',
    massLabel: 'Linear Mass Density (λ)',
    massUnit: 'kg/m',
    cUnit: 'm/s',
    hUnit: 'J·s',
    freqUnit: 'Hz (1/s)',
    energyUnit: 'Joules',
    description: 'A filament segment of length L has energy E = λ L c². Equating to E = hν gives a standard frequency ν in Hz.',
    dimensionalCheck: [
      { step: 'Starting State', value: 'E = λ L c²  and  E = hν' },
      { step: 'Equating Energy', value: 'λ L c² = hν  ⟹  ν = λ L c² / h' },
      { step: 'Unit Breakdown [λ]', value: '[λ] = kg / m' },
      { step: 'Unit Check [L]', value: '[L] = m' },
      { step: 'Unit Check [c²]', value: '[c²] = m² / s²' },
      { step: 'Left Side [λLc²]', value: '(kg / m) · m · (m² / s²) = kg · m² / s² = J' },
      { step: 'Unit Check [h]', value: '[h] = J · s = kg · m² / s' },
      { step: 'Dividing by [h]', value: 'J / (J · s) = 1/s' },
      { step: 'Solved Dimensionality', value: '[ν] = s⁻¹ = Hz' }
    ],
    vibe: 'Filamentary, linear propagation, particles locked on grid tracks.'
  },

  B: {
    id: 'B',
    name: 'Metric B: Volumetric Vorticity (ρVc²)',
    formula: 'Ω = ρ V c² / h',
    leftFormula: 'E = ρ V c²',
    rightFormula: 'E = hΩ',
    solvedFormula: 'ρ = (Ω · h) / (V · c²)',
    massLabel: 'Volumetric Density (ρ)',
    massUnit: 'kg/m³',
    cUnit: 'm/s',
    hUnit: 'J·s',
    freqUnit: 'Hz (1/s)',
    energyUnit: 'Joules',
    description: 'A volume V of density ρ has energy E = ρVc². Equating to E = hΩ gives Ω in 1/s, which can be interpreted as a vorticity/frequency-like quantity.',
    dimensionalCheck: [
      { step: 'Starting State', value: 'E = ρ V c²  and  E = hΩ' },
      { step: 'Equating Energy', value: 'ρ V c² = hΩ  ⟹  Ω = ρ V c² / h' },
      { step: 'Unit Breakdown [ρ]', value: '[ρ] = kg / m³' },
      { step: 'Unit Check [V]', value: '[V] = m³' },
      { step: 'Unit Check [c²]', value: '[c²] = m² / s²' },
      { step: 'Left Side [ρVc²]', value: '(kg / m³) · m³ · (m² / s²) = kg · m² / s² = J' },
      { step: 'Unit Check [h]', value: '[h] = J · s = kg · m² / s' },
      { step: 'Dividing by [h]', value: 'J / (J · s) = 1/s' },
      { step: 'Solved Dimensionality', value: '[Ω] = s⁻¹ = Hz' }
    ],
    vibe: 'Swirling vortexes, grid twisting, radial field distortions.'
  },

  C: {
    id: 'C',
    name: 'Metric C: Relativistic Vacuum (mc²)',
    formula: 'ν = mc² / h',
    leftFormula: 'E = mc²',
    rightFormula: 'E = hν',
    solvedFormula: 'ν = (m · c²) / h',
    massLabel: 'Rest Mass (m)',
    massUnit: 'kg',
    cUnit: 'm/s',
    hUnit: 'J·s',
    freqUnit: 'Hz (1/s)',
    energyUnit: 'Joules',
    description: 'The standard Einsteinian and Planck physics comparison. Mass is static inertia energy E=mc², matching E=hν to output clean de Broglie oscillations (Hz) without high-order acceleration.',
    dimensionalCheck: [
      { step: 'Starting State', value: 'E = mc²  and  E = hν' },
      { step: 'Equating Energy', value: 'mc² = hν  ⟹  ν = mc² / h' },
      { step: 'Unit Breakdown [m]', value: '[m] = kg' },
      { step: 'Unit Check [c²]', value: '[c²] = m² / s²' },
      { step: 'Unit Check [h]', value: '[h] = kg · m² / s' },
      { step: 'Numerator [mc²]', value: 'kg · m² / s²  (Joules)' },
      { step: 'Dividing by [h]', value: '(kg · m² / s²) / (kg · m² / s) = 1/s' },
      { step: 'Solved Dimensionality', value: '[ν] = s⁻¹ (Hz)  ⟹ PERFECT STANDARD HERTZ' }
    ],
    vibe: 'Harmonic standing waves, neat linear trajectories, classical fields.'
  }
};

export function computePhysicsValues(
  mediumId,
  frequency,
  c = 3.0,
  h = 6.626,
  gravity = 0.5,
  segmentLength = 1,
  volume = 1
) {
  const energy = frequency * h; // E = hν or E = hΩ
  let massOrDensity = 0;

  switch (mediumId) {
    case 'A':
      // ν = λ L c² / h  =>  λ = ν h / (L c²)
      massOrDensity = (frequency * h) / (segmentLength * Math.pow(c, 2));
      break;

    case 'B':
      // Ω = ρ V c² / h  =>  ρ = Ω h / (V c²)
      massOrDensity = (frequency * h) / (volume * Math.pow(c, 2));
      break;

    case 'C':
    default:
      // ν = m c² / h  =>  m = ν h / c²
      massOrDensity = (frequency * h) / Math.pow(c, 2);
      break;
  }

  // Planck mass: sqrt(ħc/G) = sqrt(hc/(2πG)) (infinite if G = 0)
  const gEff = Math.abs(gravity);
  const planckMass = gEff > 1e-6 ? Math.sqrt((h * c) / (2 * Math.PI * gEff)) : Infinity;

  // Spacetime tension (Hoop conjecture fold limit): T_st = c⁴ / (2G)
  const spacetimeTension = gEff > 1e-6 ? Math.pow(c, 4) / (2 * gEff) : Infinity;

  // Compton wavelength: λ_C = h / (m c)
  const comptonWavelength = massOrDensity > 1e-9 ? h / (massOrDensity * c) : 0;

  // U(1) Gauge coupling induced by frequency-driven local phase gradients (L' = L)
  const gaugeCoupling = Math.min(2.5, Math.abs(frequency) * 0.06);

  return {
    mass: massOrDensity,
    massOrDensity,
    energy,
    planckMass,
    spacetimeTension,
    comptonWavelength,
    gaugeCoupling
  };
}

// Bilson-Thompson / Markopoulou / Smolin 3-strand braid charge quantization: q = (t0 + t1 + t2) / 3
export function braidCharge(twists = [0, 0, 0]) {
  return (twists[0] + twists[1] + twists[2]) / 3;
}

export function computeEngineMetrics(mediumId, frequency, c = 3.0, h = 6.626, gravity = 0.5) {
  const res = computePhysicsValues(mediumId, frequency, c, h, gravity);
  return {
    nodeDensity: res.massOrDensity,
    kineticPower: res.energy,
    clumpThreshold: res.planckMass,
    spacetimeTension: res.spacetimeTension,
    gaugeCoupling: res.gaugeCoupling
  };
}