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

/////

/**
 * Defines the **sole** asynchronous test of a file.
 * @see {@link ../docs/rfcs/async-helpers.md} for background.
 *
 * @param {Function} testFunc a callback whose returned promise indicates test results
 *   (fulfillment for success, rejection for failure)
 * @returns {void}
 */
function asyncTest(testFunc) {
  if (!Object.hasOwn(globalThis, "$DONE")) {
    throw new Test262Error("asyncTest called without async flag");
  }
  if (typeof testFunc !== "function") {
    $DONE(new Test262Error("asyncTest called with non-function argument"));
    return;
  }
  try {
    testFunc().then(
      function () {
        $DONE();
      },
      function (error) {
        $DONE(error);
      }
    );
  } catch (syncError) {
    $DONE(syncError);
  }
}

/**
 * Asserts that a callback asynchronously throws an instance of a particular
 * error (i.e., returns a promise whose rejection value is an object referencing
 * the constructor).
 *
 * @param {Function} expectedErrorConstructor the expected constructor of the
 *   rejection value
 * @param {Function} func the callback
 * @param {string} [message] the prefix to use for failure messages
 * @returns {Promise<void>} fulfills if the expected error is thrown,
 *   otherwise rejects
 */
assert.throwsAsync = function (expectedErrorConstructor, func, message) {
  return new Promise(function (resolve) {
    var fail = function (detail) {
      if (message === undefined) {
        throw new Test262Error(detail);
      }
      throw new Test262Error(message + " " + detail);
    };
    if (typeof expectedErrorConstructor !== "function") {
      fail(
        "assert.throwsAsync called with an argument that is not an error constructor"
      );
    }
    if (typeof func !== "function") {
      fail("assert.throwsAsync called with an argument that is not a function");
    }
    var expectedName = expectedErrorConstructor.name;
    var expectation =
      "Expected a " + expectedName + " to be thrown asynchronously";
    var res;
    try {
      res = func();
    } catch (thrown) {
      fail(expectation + " but the function threw synchronously");
    }
    if (
      res === null ||
      typeof res !== "object" ||
      typeof res.then !== "function"
    ) {
      fail(expectation + " but result was not a thenable");
    }
    var onResFulfilled, onResRejected;
    var resSettlementP = new Promise(function (onFulfilled, onRejected) {
      onResFulfilled = onFulfilled;
      onResRejected = onRejected;
    });
    try {
      res.then(onResFulfilled, onResRejected);
    } catch (thrown) {
      fail(expectation + " but .then threw synchronously");
    }
    resolve(
      resSettlementP.then(
        function () {
          fail(expectation + " but no exception was thrown at all");
        },
        function (thrown) {
          var actualName;
          if (thrown === null || typeof thrown !== "object") {
            fail(expectation + " but thrown value was not an object");
          } else if (thrown.constructor !== expectedErrorConstructor) {
            actualName = thrown.constructor.name;
            if (expectedName === actualName) {
              fail(
                expectation +
                  " but got a different error constructor with the same name"
              );
            }
            fail(expectation + " but got a " + actualName);
          }
        }
      )
    );
  });
};

function __consolePrintHandle__(msg) {
  print(msg);
}

function $DONE(error) {
  if (error) {
    if (typeof error === "object" && error !== null && "name" in error) {
      __consolePrintHandle__(
        "Test262:AsyncTestFailure:" + error.name + ": " + error.message
      );
    } else {
      __consolePrintHandle__(
        "Test262:AsyncTestFailure:Test262Error: " + String(error)
      );
    }
  } else {
    __consolePrintHandle__("Test262:AsyncTestComplete");
  }
}

// Copyright (C) 2022 Igalia, S.L. All rights reserved.
// This code is governed by the BSD license found in the LICENSE file.
/*---
description: |
    The 'asyncTest' helper calls $DONE with the rejection value if the test function rejects.
flags: [async]
includes: [asyncHelpers.js, compareArray.js]
---*/
const rejectionValues = [];
var realDone = $DONE;
globalThis.$DONE = function (mustBeDefined) {
  rejectionValues.push(mustBeDefined);
};
const someObject = {};

(async function () {
  asyncTest(function () {
    return Promise.reject(null);
  });
})();
// .then(() => {
//   asyncTest(function () {
//     return Promise.reject(someObject);
//   });
// })
// .then(() => {
//   asyncTest(function () {
//     return Promise.reject("hi");
//   });
// })
// .then(() => {
//   asyncTest(function () {
//     return Promise.reject(10);
//   });
// })
// .then(() => {
//   asyncTest(function () {
//     return {
//       then(res, rej) {
//         rej(true);
//       },
//     };
//   });
// })
// .then(() => {
//   assert.compareArray(rejectionValues, [null, someObject, "hi", 10, true]);
// })
// .then(realDone, realDone);
