// The sign-in pages Kratos sends the browser to (selfservice.flows.*.ui_url
// in k8s/kratos/configmap.yaml): /auth/login, /auth/register,
// /auth/settings and /auth/error. Each draws its Kratos flow from the flow's
// own UI nodes, so every method Kratos offers appears with no code here:
// the email code and "Sign in with Basable" (single sign-on
// with every app on Basable). A form posts straight to Kratos, which
// answers with the next step of the same flow or with where to go next.
//
// There is no file at /auth/...: the bucket answers an unknown path with
// index.html, which loads this script, and the script takes over <main>
// when the path is one of these.
(function () {
  var parts = location.pathname.split('/');
  if (parts[1] !== 'auth') return;
  var kind = parts[2] || 'login';
  var main = document.querySelector('main');
  var params = new URLSearchParams(location.search);
  var flows = { login: 'login', register: 'registration', settings: 'settings' };
  var titles = { login: 'Sign in', register: 'Create an account', settings: 'Account settings' };

  function el(tag, props, children) {
    var node = document.createElement(tag);
    Object.keys(props || {}).forEach(function (k) {
      if (props[k] !== undefined && props[k] !== null) node[k] = props[k];
    });
    (children || []).forEach(function (c) {
      node.append(typeof c === 'string' ? document.createTextNode(c) : c);
    });
    return node;
  }

  function show(children) {
    main.replaceChildren.apply(main, children);
  }

  function notices(messages) {
    return (messages || []).map(function (m) {
      return el('p', { className: 'notice notice-' + m.type }, [m.text]);
    });
  }

  function restart(api) {
    location.replace('/.ory/self-service/' + api + '/browser');
  }

  if (kind === 'error') {
    fetch('/.ory/self-service/errors?id=' + encodeURIComponent(params.get('id') || ''), { credentials: 'include' })
      .then(function (res) { return res.json(); })
      .then(function (body) {
        var e = body.error || {};
        show([
          el('h1', {}, ['Sign-in failed']),
          el('p', {}, [e.reason || e.message || 'Something went wrong.']),
          el('a', { href: '/.ory/self-service/login/browser' }, ['Try again']),
        ]);
      })
      .catch(function () {
        show([el('h1', {}, ['Sign-in failed']), el('a', { href: '/.ory/self-service/login/browser' }, ['Try again'])]);
      });
    return;
  }

  var api = flows[kind];
  if (!api) {
    location.replace('/');
    return;
  }
  var id = params.get('flow');
  if (!id) {
    restart(api);
    return;
  }

  // A node is one input of the flow: hidden (the CSRF token, the flow's
  // state), a field (the email, the code) or a submit button (a method,
  // or a provider such as Basable). Passkey nodes need WebAuthn's script
  // and are left out here.
  function field(node) {
    var a = node.attributes;
    if (node.type === 'text') return el('p', {}, [a.text && a.text.text]);
    if (node.type !== 'input' || node.group === 'passkey' || node.group === 'webauthn') return null;
    var label = node.meta && node.meta.label ? node.meta.label.text : a.name;
    if (a.type === 'hidden') return el('input', { type: 'hidden', name: a.name, value: a.value });
    if (a.type === 'submit' || a.type === 'button') {
      return el('button', { type: 'submit', name: a.name, value: a.value, disabled: a.disabled }, [label]);
    }
    return el('label', {}, [
      label,
      el('input', {
        type: a.type, name: a.name, value: a.value || '', required: a.required,
        disabled: a.disabled, autocomplete: a.autocomplete,
      }),
    ].concat(notices(node.messages)));
  }

  fetch('/.ory/self-service/' + api + '/flows?id=' + encodeURIComponent(id), {
    credentials: 'include',
    headers: { Accept: 'application/json' },
  })
    .then(function (res) {
      // An expired, foreign or unknown flow starts over.
      if (res.status === 403 || res.status === 404 || res.status === 410) {
        restart(api);
        return null;
      }
      return res.json();
    })
    .then(function (flow) {
      if (!flow) return;
      var form = el('form', { action: flow.ui.action, method: flow.ui.method, className: 'auth' },
        flow.ui.nodes.map(field).filter(Boolean));
      var other = kind === 'login'
        ? el('a', { href: '/.ory/self-service/registration/browser' }, ['Create an account'])
        : kind === 'register'
          ? el('a', { href: '/.ory/self-service/login/browser' }, ['Sign in instead'])
          : el('a', { href: '/' }, ['Back']);
      show([el('h1', {}, [titles[kind]])].concat(notices(flow.ui.messages), [form, other]));
    })
    .catch(function () {
      show([el('h1', {}, [titles[kind]]), el('p', {}, ['Kratos is not reachable yet.'])]);
    });
})();
