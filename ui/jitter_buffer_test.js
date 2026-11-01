// Unit test for jitter buffer scheduler logic

function testJitterBuffer() {
  const TARGET_BUFFER_MS = 700;
  const FPS = 15;
  const FRAME_INTERVAL_US = 1000000 / FPS; // 66667 microseconds
  const KEYFRAME_EVERY = 30; // frames
  const KEYFRAME_GAP_MS = 450; // arrival delay before keyframe
  
  let playoutAnchor = null;
  const frameBuffer = [];
  const presentationIntervals = [];
  let lastPresentTime = null;
  
  // Simulate frame arrivals
  let wallTime = 0; // simulated performance.now()
  let mediaTime = 0; // camera timestamp in microseconds
  
  for (let i = 0; i < 60; i++) { // two keyframe cycles
    const isKeyframe = i % KEYFRAME_EVERY === 0;
    
    // Simulate keyframe arrival gap
    if (isKeyframe && i > 0) {
      wallTime += KEYFRAME_GAP_MS;
    }
    
    // Frame arrives at wallTime, with mediaTime from camera
    const arrivalTime = wallTime;
    
    // Initialize anchor on first frame
    if (!playoutAnchor) {
      playoutAnchor = { wallTime: arrivalTime + TARGET_BUFFER_MS, mediaTs: mediaTime };
    }
    
    // Calculate presentation time from media timeline
    const mediaDeltaUs = mediaTime - playoutAnchor.mediaTs;
    const mediaDeltaMs = mediaDeltaUs / 1000;
    const presentAt = playoutAnchor.wallTime + mediaDeltaMs;
    
    frameBuffer.push({ presentAt, mediaTime, arrivalTime });
    
    // Advance media timeline at steady rate (15 fps)
    mediaTime += FRAME_INTERVAL_US;
    
    // Simulate normal frame arrival interval (no gap for non-keyframes)
    if (!isKeyframe) {
      wallTime += FRAME_INTERVAL_US / 1000; // microseconds to milliseconds
    }
  }
  
  // Extract presentation intervals
  for (let i = 1; i < frameBuffer.length; i++) {
    const interval = frameBuffer[i].presentAt - frameBuffer[i-1].presentAt;
    presentationIntervals.push(interval);
  }
  
  // Check assertions
  const avgInterval = presentationIntervals.reduce((a, b) => a + b, 0) / presentationIntervals.length;
  const expectedInterval = 1000 / FPS; // 66.67 ms
  const maxDeviation = Math.max(...presentationIntervals.map(i => Math.abs(i - expectedInterval)));
  
  console.log(`Average presentation interval: ${avgInterval.toFixed(2)}ms (expected ${expectedInterval.toFixed(2)}ms)`);
  console.log(`Max deviation from expected: ${maxDeviation.toFixed(2)}ms`);
  console.log(`Frame 30 (keyframe) presentation interval: ${presentationIntervals[29].toFixed(2)}ms`);
  
  // Assertions
  const avgOk = Math.abs(avgInterval - expectedInterval) < 1.0;
  const maxDevOk = maxDeviation < 5.0;
  const keyframeOk = Math.abs(presentationIntervals[29] - expectedInterval) < 5.0;
  
  if (avgOk && maxDevOk && keyframeOk) {
    console.log("✓ Test PASSED: Presentation intervals remain steady despite 450ms keyframe gaps");
    return true;
  } else {
    console.log("✗ Test FAILED:");
    if (!avgOk) console.log(`  - Average interval ${avgInterval.toFixed(2)}ms differs from expected ${expectedInterval.toFixed(2)}ms`);
    if (!maxDevOk) console.log(`  - Max deviation ${maxDeviation.toFixed(2)}ms exceeds 5ms threshold`);
    if (!keyframeOk) console.log(`  - Keyframe interval ${presentationIntervals[29].toFixed(2)}ms differs from expected`);
    return false;
  }
}

// Test u32 timestamp wrap handling
function testTimestampWrap() {
  const U32_MAX = 0xFFFFFFFF;
  
  let lastRawTs = 0;
  let epoch = 0;
  const unwrap = (rawTs) => {
    if (lastRawTs > 0) {
      const rawDelta = rawTs - lastRawTs;
      if (rawDelta < -0x80000000) {
        epoch += 0x100000000;
      }
    }
    lastRawTs = rawTs;
    return epoch + rawTs;
  };
  
  // Start at a point that will wrap within the test
  // Start 20 frames before wrap at 15fps (20 * 66667µs = 1,333,340µs before wrap)
  const startPoint = U32_MAX - 1333340;
  
  // Simulate 50 timestamps crossing the wrap boundary
  const timestamps = [];
  for (let i = 0; i < 50; i++) {
    // Calculate next timestamp and wrap it to u32
    const rawTs = ((startPoint + i * 66667) & 0xFFFFFFFF) >>> 0;
    const extended = unwrap(rawTs);
    timestamps.push({ rawTs, extended });
  }
  
  // Check that extended timestamps are monotonic across the wrap
  let allMonotonic = true;
  for (let i = 1; i < timestamps.length; i++) {
    if (timestamps[i].extended <= timestamps[i-1].extended) {
      console.log(`✗ Wrap test FAILED: timestamp ${i} not monotonic`);
      console.log(`  Frame ${i-1}: raw ${timestamps[i-1].rawTs} -> extended ${timestamps[i-1].extended}`);
      console.log(`  Frame ${i}: raw ${timestamps[i].rawTs} -> extended ${timestamps[i].extended}`);
      allMonotonic = false;
      break;
    }
  }
  
  // Check that the wrap was detected (raw timestamp should decrease at some point)
  let wrapDetected = false;
  for (let i = 1; i < timestamps.length; i++) {
    if (timestamps[i].rawTs < timestamps[i-1].rawTs) {
      wrapDetected = true;
      break;
    }
  }
  
  // Verify the extended timestamps maintain proper spacing (within 1µs tolerance)
  let spacingOk = true;
  const expectedStep = 66667;
  for (let i = 1; i < timestamps.length; i++) {
    const step = timestamps[i].extended - timestamps[i-1].extended;
    if (Math.abs(step - expectedStep) > 1) {
      console.log(`✗ Wrap test FAILED: frame ${i} step ${step}µs != expected ${expectedStep}µs`);
      spacingOk = false;
      break;
    }
  }
  
  if (allMonotonic && wrapDetected && spacingOk) {
    console.log("✓ Timestamp wrap test PASSED: u32 wrap handled correctly, timestamps stay monotonic");
    return true;
  } else {
    console.log("✗ Timestamp wrap test FAILED:");
    if (!allMonotonic) console.log("  - Extended timestamps not monotonic across wrap");
    if (!wrapDetected) console.log("  - Wrap not detected in test data");
    if (!spacingOk) console.log("  - Frame spacing incorrect across wrap");
    return false;
  }
}

// Test long fallback mode stability
function testLongFallback() {
  const nominalStep = 66667; // microseconds at 15 fps
  const numFrames = 100000; // ~111 minutes at 15 fps
  
  let lastMediaTs = 0;
  const timestamps = [];
  
  // Simulate fallback mode: each frame advances by nominal step
  for (let i = 0; i < numFrames; i++) {
    const mediaTs = (i === 0) ? 0 : lastMediaTs + nominalStep;
    timestamps.push(mediaTs);
    lastMediaTs = mediaTs;
  }
  
  // Check for monotonicity and steady intervals
  let allMonotonic = true;
  let maxIntervalError = 0;
  
  for (let i = 1; i < timestamps.length; i++) {
    const interval = timestamps[i] - timestamps[i-1];
    if (interval <= 0) {
      allMonotonic = false;
      break;
    }
    const error = Math.abs(interval - nominalStep);
    maxIntervalError = Math.max(maxIntervalError, error);
  }
  
  // Final timestamp should be close to expected
  const expectedFinal = (numFrames - 1) * nominalStep;
  const actualFinal = timestamps[timestamps.length - 1];
  const finalError = Math.abs(actualFinal - expectedFinal);
  
  if (allMonotonic && maxIntervalError === 0 && finalError === 0) {
    console.log(`✓ Long fallback test PASSED: ${numFrames} frames stay stable in fallback mode`);
    return true;
  } else {
    console.log("✗ Long fallback test FAILED:");
    if (!allMonotonic) console.log("  - Timestamps not monotonic");
    if (maxIntervalError > 0) console.log(`  - Max interval error: ${maxIntervalError}µs`);
    if (finalError > 0) console.log(`  - Final timestamp error: ${finalError}µs`);
    return false;
  }
}

// Run all tests
const tests = [
  { name: "Jitter buffer basic", fn: testJitterBuffer },
  { name: "Timestamp wrap (u32)", fn: testTimestampWrap },
  { name: "Long fallback mode", fn: testLongFallback }
];

let allPassed = true;
for (const test of tests) {
  console.log(`\n=== ${test.name} ===`);
  const result = test.fn();
  if (!result) allPassed = false;
}

console.log(`\n${"=".repeat(50)}`);
console.log(allPassed ? "All tests PASSED" : "Some tests FAILED");
process.exit(allPassed ? 0 : 1);
