// Code that reimplements what the library already offers.
//
// Each of these is written the way somebody actually writes it, not the way the
// engine implements it. That is the whole claim being tested: the two meet in
// normal form without either side being written for the other.

export function firstAdmin(users: { role: string }[]) {
  for (let i = 0; i < users.length; i++) {
    const user = users[i];
    if (user.role === "admin") {
      return user;
    }
  }
  return undefined;
}

export function lastFailure(events: { ok: boolean }[]) {
  for (let i = events.length - 1; i >= 0; i--) {
    const event = events[i];
    if (!event.ok) {
      return event;
    }
  }
  return undefined;
}

// Not a search: it visits every element and keeps going. The library's `find`
// must not match this, and a matcher that ignored the early return would.
export function countAdmins(users: { role: string }[]) {
  let total = 0;
  for (let i = 0; i < users.length; i++) {
    if (users[i].role === "admin") {
      total = total + 1;
    }
  }
  return total;
}
