# Example: Automated Login Flow

This example demonstrates how to automate a login flow using debug-bridge.

## Setup

```javascript
const WebSocket = require('ws');

const SESSION_ID = 'login-test';
const ws = new WebSocket(`ws://localhost:4000/debug?role=agent&sessionId=${SESSION_ID}`);

// Helper functions
function send(cmd) {
  return new Promise((resolve) => {
    const requestId = `cmd-${Date.now()}`;
    const handler = (data) => {
      const msg = JSON.parse(data.toString());
      if (msg.requestId === requestId) {
        ws.off('message', handler);
        resolve(msg);
      }
    };
    ws.on('message', handler);

    ws.send(JSON.stringify({
      protocolVersion: 1,
      sessionId: SESSION_ID,
      timestamp: Date.now(),
      requestId,
      ...cmd
    }));
  });
}

function delay(ms) {
  return new Promise(resolve => setTimeout(resolve, ms));
}
```

## Login Flow

```javascript
async function loginFlow() {
  // Wait for connection
  await new Promise(resolve => ws.on('open', resolve));
  console.log('Connected to debug-bridge');

  // 1. Get UI tree to find elements
  console.log('Getting UI tree...');
  const uiTree = await send({ type: 'request_ui_tree' });
  const items = uiTree.items || [];
  console.log(`Found ${items.length} interactive elements`);

  // 2. Find login form elements
  const emailInput = items.find(i =>
    i.role === 'input' &&
    (i.meta?.type === 'email' || i.meta?.placeholder?.toLowerCase().includes('email'))
  );

  const passwordInput = items.find(i =>
    i.role === 'input' &&
    (i.meta?.type === 'password' || i.meta?.placeholder?.toLowerCase().includes('password'))
  );

  const loginButton = items.find(i =>
    (i.role === 'button' || i.meta?.tagName === 'button') &&
    (i.text?.toLowerCase().includes('sign') || i.text?.toLowerCase().includes('log'))
  );

  if (!emailInput || !passwordInput || !loginButton) {
    console.error('Could not find login form elements');
    console.log('Available elements:', items.map(i => ({ id: i.stableId, role: i.role, text: i.text })));
    return;
  }

  console.log('Found elements:');
  console.log('  Email:', emailInput.stableId);
  console.log('  Password:', passwordInput.stableId);
  console.log('  Button:', loginButton.stableId);

  // 3. Click and type email
  console.log('Entering email...');
  await send({ type: 'click', target: { stableId: emailInput.stableId } });
  await delay(100);
  await send({
    type: 'type',
    target: { stableId: emailInput.stableId },
    text: 'user@example.com',
    options: { clear: true }
  });

  // 4. Click and type password
  console.log('Entering password...');
  await send({ type: 'click', target: { stableId: passwordInput.stableId } });
  await delay(100);
  await send({
    type: 'type',
    target: { stableId: passwordInput.stableId },
    text: 'password123',
    options: { clear: true }
  });

  // 5. Take screenshot before submit
  console.log('Taking pre-submit screenshot...');
  await send({ type: 'request_screenshot' });

  // 6. Click login button
  console.log('Clicking login button...');
  await send({ type: 'click', target: { stableId: loginButton.stableId } });

  // 7. Wait for navigation/response
  console.log('Waiting for login response...');
  await delay(3000);

  // 8. Check state for auth tokens
  console.log('Checking auth state...');
  const stateResult = await send({ type: 'request_state' });

  // 9. Take final screenshot
  console.log('Taking post-login screenshot...');
  await send({ type: 'request_screenshot' });

  // 10. Check if login succeeded
  const uiTreeAfter = await send({ type: 'request_ui_tree' });
  const stillOnLogin = uiTreeAfter.items?.some(i =>
    i.text?.toLowerCase().includes('sign in') ||
    i.text?.toLowerCase().includes('log in')
  );

  if (stillOnLogin) {
    console.log('Login may have failed - still on login page');
  } else {
    console.log('Login appears successful!');
  }

  ws.close();
}

loginFlow().catch(console.error);
```

## Running the Example

1. Start debug-bridge server:
   ```bash
   npx debug-bridge-cli connect --session login-test
   ```

2. Start your app with debug params:
   ```bash
   # Your app should open at:
   # http://localhost:5173?session=login-test&port=4000
   ```

3. Run the login script:
   ```bash
   node login-flow.js
   ```

## Expected Output

```
Connected to debug-bridge
Getting UI tree...
Found 6 interactive elements
Found elements:
  Email: input-email
  Password: input-password
  Button: button-signin
Entering email...
Entering password...
Taking pre-submit screenshot...
Clicking login button...
Waiting for login response...
Checking auth state...
Taking post-login screenshot...
Login appears successful!
```
