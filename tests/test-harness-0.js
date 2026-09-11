function assert(mustBeTrue, message) {
  if (mustBeTrue === true) {
    return;
  }

  if (message === undefined) {
    message = "Expected true but got " + assert._toString(mustBeTrue);
  }
  throw new Test262Error(message);
}

assert._isSameValue = function (a, b) {
  if (a === b) {
    // Handle +/-0 vs. -/+0
    return a !== 0 || 1 / a === 1 / b;
  }

  // Handle NaN vs. NaN
  return a !== a && b !== b;
};

assert.sameValue = function (actual, expected, message) {
  try {
    if (assert._isSameValue(actual, expected)) {
      return;
    }
  } catch (error) {
    throw new Test262Error(
      message + " (_isSameValue operation threw) " + error
    );
    return;
  }

  if (message === undefined) {
    message = "";
  } else {
    message += " ";
  }

  message +=
    "Expected SameValue(«" +
    assert._toString(actual) +
    "», «" +
    assert._toString(expected) +
    "») to be true";

  throw new Test262Error(message);
};

assert.notSameValue = function (actual, unexpected, message) {
  if (!assert._isSameValue(actual, unexpected)) {
    return;
  }

  if (message === undefined) {
    message = "";
  } else {
    message += " ";
  }

  message +=
    "Expected SameValue(«" +
    assert._toString(actual) +
    "», «" +
    assert._toString(unexpected) +
    "») to be false";

  throw new Test262Error(message);
};

assert.throws = function (expectedErrorConstructor, func, message) {
  var expectedName, actualName;
  if (typeof func !== "function") {
    throw new Test262Error(
      "assert.throws requires two arguments: the error constructor " +
        "and a function to run"
    );
    return;
  }
  if (message === undefined) {
    message = "";
  } else {
    message += " ";
  }

  try {
    func();
  } catch (thrown) {
    if (typeof thrown !== "object" || thrown === null) {
      message += "Thrown value was not an object!";
      throw new Test262Error(message);
    } else if (thrown.constructor !== expectedErrorConstructor) {
      expectedName = expectedErrorConstructor.name;
      actualName = thrown.constructor.name;
      if (expectedName === actualName) {
        message +=
          "Expected a " +
          expectedName +
          " but got a different error constructor with the same name";
      } else {
        message += "Expected a " + expectedName + " but got a " + actualName;
      }
      throw new Test262Error(message);
    }
    return;
  }

  message +=
    "Expected a " +
    expectedErrorConstructor.name +
    " to be thrown but no exception was thrown at all";
  throw new Test262Error(message);
};

assert._formatIdentityFreeValue = function formatIdentityFreeValue(value) {
  switch (value === null ? "null" : typeof value) {
    case "string":
      return typeof JSON !== "undefined" ? JSON.stringify(value) : `"${value}"`;
    case "bigint":
      return `${value}n`;
    case "number":
      if (value === 0 && 1 / value === -Infinity) return "-0";
    // falls through
    case "boolean":
    case "undefined":
    case "null":
      return String(value);
  }
};

assert._toString = function (value) {
  var basic = assert._formatIdentityFreeValue(value);
  if (basic) return basic;
  try {
    return String(value);
  } catch (err) {
    if (err.name === "TypeError") {
      return Object.prototype.toString.call(value);
    }
    throw err;
  }
};

function Test262Error(message) {
  this.message = message || "";
}

Test262Error.prototype.toString = function () {
  return "Test262Error: " + this.message;
};

Test262Error.thrower = function (message) {
  throw new Test262Error(message);
};

function $DONOTEVALUATE() {
  throw "Test262: This statement should not be evaluated.";
}

function $DETACHBUFFER(buffer) {
  if (!$262 || typeof $262.detachArrayBuffer !== "function") {
    throw new Test262Error("No method available to detach an ArrayBuffer");
  }
  $262.detachArrayBuffer(buffer);
}

// Copyright (C) 2017 Rick Waldron. All rights reserved.
// This code is governed by the BSD license found in the LICENSE file.
/*---
description: >
    Including detachArrayBuffer.js will expose a function:

        $DETACHBUFFER

    $DETACHBUFFER relies on the presence of a host definition for $262.detachArrayBuffer

includes: [detachArrayBuffer.js]
---*/

var $262 = {
  detachArrayBuffer() {
    throw new Test262Error("$262.detachArrayBuffer called.");
  },
  destroy() {},
};

var ab = new ArrayBuffer(1);
var threw = false;

try {
  $DETACHBUFFER(ab);
} catch (err) {
  threw = true;
  if (err.constructor !== Test262Error) {
    throw new Error(
      'Expected a Test262Error, but a "' +
        err.constructor.name +
        '" was thrown.'
    );
  }
  if (err.message !== "$262.detachArrayBuffer called.") {
    throw new Error(`Expected error message: ${err.message}`);
  }
}

if (threw === false) {
  throw new Error("Expected a Test262Error, but no error was thrown.");
}
