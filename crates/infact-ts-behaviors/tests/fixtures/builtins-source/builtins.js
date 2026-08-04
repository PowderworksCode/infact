// A stand-in for an engine's self-hosted builtins.
//
// Purpose-built rather than fetched: the real source is MPL-2.0 and lives
// outside the repository, and a collision test wants pairs chosen for what they
// would erase rather than whatever a fetch happened to bring. Each pair below
// differs in exactly one thing a normalizer has been caught dropping.
//
// The shapes are the ones the specification is written in — an index walk, a
// coercion of the receiver, a precondition throw — because a behavior derived
// from something a caller never writes is worth nothing.

// find / findLast: the same search, opposite directions. Encoding direction as
// something wrapped around the sequence puts it where a hole absorbs it.
function ArrayFind(predicate) {
  var O = ToObject(this);
  var len = ToLength(O.length);
  if (!IsCallable(predicate)) {
    ThrowTypeError(JSMSG_NOT_FUNCTION);
  }
  for (var k = 0; k < len; k++) {
    var kValue = O[k];
    if (callContentFunction(predicate, undefined, kValue, k, O)) {
      return kValue;
    }
  }
  return undefined;
}

function ArrayFindLast(predicate) {
  var O = ToObject(this);
  var len = ToLength(O.length);
  if (!IsCallable(predicate)) {
    ThrowTypeError(JSMSG_NOT_FUNCTION);
  }
  for (var k = len - 1; k >= 0; k--) {
    var kValue = O[k];
    if (callContentFunction(predicate, undefined, kValue, k, O)) {
      return kValue;
    }
  }
  return undefined;
}

// some / every: one reports that a match exists, the other that none is
// missing. `true` and `()` were once one form, which made these the same thing.
function ArraySome(callbackfn) {
  var O = ToObject(this);
  var len = ToLength(O.length);
  for (var k = 0; k < len; k++) {
    if (callContentFunction(callbackfn, undefined, O[k], k, O)) {
      return true;
    }
  }
  return false;
}

function ArrayEvery(callbackfn) {
  var O = ToObject(this);
  var len = ToLength(O.length);
  for (var k = 0; k < len; k++) {
    if (!callContentFunction(callbackfn, undefined, O[k], k, O)) {
      return false;
    }
  }
  return true;
}

// map / filter: one cannot drop an element and the other cannot change one.
// A single "transform methods" list made them one behavior.
function ArrayMap(callbackfn) {
  var O = ToObject(this);
  var len = ToLength(O.length);
  var A = [];
  for (var k = 0; k < len; k++) {
    A[k] = callContentFunction(callbackfn, undefined, O[k], k, O);
  }
  return A;
}

function ArrayFilter(callbackfn) {
  var O = ToObject(this);
  var len = ToLength(O.length);
  var A = [];
  for (var k = 0; k < len; k++) {
    var kValue = O[k];
    if (callContentFunction(callbackfn, undefined, kValue, k, O)) {
      A.push(kValue);
    }
  }
  return A;
}

// findIndex / find: the position against the element. Both walk forward and
// stop at the first match, and only what they hand back separates them.
function ArrayFindIndex(predicate) {
  var O = ToObject(this);
  var len = ToLength(O.length);
  for (var k = 0; k < len; k++) {
    if (callContentFunction(predicate, undefined, O[k], k, O)) {
      return k;
    }
  }
  return -1;
}

// A wrapper with no behavior of its own, so that following a delegation is
// exercised: what the public name describes is what the helper describes.
//
// The call is the whole body. A call buried inside an expression -- the
// `indexOf(x) !== -1` that `includes` is usually written as -- is NOT followed,
// by either frontend: `delegation_target` is a shape test over the whole form.
// That is a real gap and it is recorded rather than papered over here.
function ArrayIndexOf(searchElement) {
  return ArrayIndexOfInternal(ToObject(this), searchElement);
}

function ArrayIndexOfInternal(O, searchElement) {
  var len = ToLength(O.length);
  for (var k = 0; k < len; k++) {
    if (O[k] === searchElement) {
      return k;
    }
  }
  return -1;
}

// A lazy adaptor: the callable constructs a type and the work is in that type's
// `next`. This is the one route that licenses the one-step lift.
class ArrayIterator {
  constructor(array) {
    this.array = array;
    this.index = 0;
  }

  next() {
    var array = this.array;
    var len = ToLength(array.length);
    for (var k = this.index; k < len; k++) {
      if (array[k] !== undefined) {
        return { value: array[k], done: false };
      }
    }
    return { value: undefined, done: true };
  }
}

function ArrayValues() {
  return new ArrayIterator(ToObject(this));
}
