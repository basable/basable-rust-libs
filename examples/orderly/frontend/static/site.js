// Plain JavaScript: the session state from Kratos, the health of the API.
(async function () {
  const status = document.getElementById('status');
  try {
    const res = await fetch('/api/health.v1.HealthService/Check', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: '{}',
    });
    const body = await res.json();
    status.textContent = body.ready ? 'The API is ready.' : 'The API is starting…';
  } catch (e) {
    status.textContent = 'The API is not reachable yet.';
  }
  try {
    const who = await fetch('/.ory/sessions/whoami', { credentials: 'include' });
    const signedIn = who.ok;
    document.querySelectorAll('[data-when]').forEach((el) => {
      el.style.display = el.dataset.when === (signedIn ? 'signed-in' : 'anonymous') ? 'inline' : 'none';
    });
  } catch (e) {
    // Kratos absent (auth: none): leave the links hidden.
  }
})();
