// Unit test for jitter buffer scheduler logic
// Simulates 15 fps with 450ms gap before every 30th frame (keyframe)

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

// Run test
const result = testJitterBuffer();
process.exit(result ? 0 : 1);
