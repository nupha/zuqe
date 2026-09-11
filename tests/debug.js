var abruptLength = new Proxy([], {
  get: function (_target, key) {
    if (key === "length") {
      throw new Error();
    }
  },
});

JSON.stringify(null, abruptLength);

// console.log(Error, function () {
//   JSON.stringify(null, abruptLength);
// });

var abruptToPrimitive = {
  valueOf: function () {
    throw new Error();
  },
};

var abruptToLength = new Proxy([], {
  get: function (_target, key) {
    if (key === "length") {
      return abruptToPrimitive;
    }
  },
});

JSON.stringify([], abruptToLength);

// console.log(Error, function () {
//   JSON.stringify([], abruptToLength);
// });

var abruptIndex = new Array(1);
Object.defineProperty(abruptIndex, "0", {
  get: function () {
    throw new Error();
  },
});

// console.log(Error, function () {
//   JSON.stringify({}, abruptIndex);
// });
