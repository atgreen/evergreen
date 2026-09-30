;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;;; A signed-distance-field scene, written as Lisp data, compiled to GLSL.
;;;;
;;;; The scene below is a list. Walking it emits the `map()` function of a
;;;; raymarcher, so the 3D world is not described BY Lisp — it IS Lisp, and a
;;;; change to the list is a change to the shader.
;;;;
;;;; Every node compiles to a GLSL expression for the distance from a point to
;;;; the surface. The point itself is threaded down as an expression too, which
;;;; is what lets `repeat` fold infinite copies of a subtree into one modulo.

(defparameter *sdf-uniforms* '()
  "Symbols met while compiling the scene, in the order they appeared. Each one
became a GLSL uniform instead of a constant, so Lisp can move it per frame.")

(defun uniform-name (symbol)
  (substitute #\_ #\- (string-downcase (symbol-name symbol))))

(defun glf (x)
  "A GLSL float: a number becomes a literal, a SYMBOL becomes a uniform.

That one line is what puts Lisp in charge of the scene rather than merely
generating it once — write a symbol where a number would go and the value
becomes live, driven from the render loop."
  (cond ((numberp x) (format nil "~,5F" (float x 1.0)))
        ((symbolp x)
         (pushnew x *sdf-uniforms*)
         (format nil "u_~A" (uniform-name x)))
        (t (error "not a GLSL number: ~S" x))))

(defun glvec3 (xyz)
  (format nil "vec3(~A,~A,~A)" (glf (first xyz)) (glf (second xyz)) (glf (third xyz))))

(defun sdf-at (node)
  "The :at offset of a node, defaulting to the origin."
  (let ((tail (member :at node)))
    (if tail (second tail) '(0 0 0))))

(defun sdf-emit (node p)
  "GLSL expression for the distance from point-expression P to NODE."
  (let ((head (car node))
        (args (cdr node)))
    (case head
      ;; ── primitives ────────────────────────────────────────────────
      (sphere (format nil "(length(~A-~A)-~A)" p (glvec3 (sdf-at node)) (glf (first args))))
      (box    (format nil "sdBox(~A-~A,~A)" p (glvec3 (sdf-at node)) (glvec3 (first args))))
      (torus  (format nil "sdTorus(~A-~A,vec2(~A,~A))" p (glvec3 (sdf-at node))
                      (glf (first args)) (glf (second args))))
      (plane  (format nil "(~A.y-(~A))" p (glf (first args))))
      ;; ── combinators ───────────────────────────────────────────────
      (union
       (reduce (lambda (a b) (format nil "min(~A,~A)" a b))
               (mapcar (lambda (n) (sdf-emit n p)) args)))
      (smooth-union
       (let ((k (glf (first args))))
         (reduce (lambda (a b) (format nil "smin(~A,~A,~A)" a b k))
                 (mapcar (lambda (n) (sdf-emit n p)) (rest args)))))
      (subtract
       (format nil "max(~A,-(~A))" (sdf-emit (first args) p) (sdf-emit (second args) p)))
      (intersect
       (format nil "max(~A,~A)" (sdf-emit (first args) p) (sdf-emit (second args) p)))
      ;; ── domain operators: these rewrite the POINT, not the distance ─
      (repeat
       (sdf-emit (second args) (format nil "opRep(~A,~A)" p (glvec3 (first args)))))
      (twist
       (sdf-emit (second args) (format nil "opTwist(~A,~A)" p (glf (first args)))))
      ;; Shifts the DOMAIN, which is how a repetition lattice is moved off the
      ;; origin: `repeat` folds around multiples of the cell size, so without
      ;; this there is always a copy sitting at (0,0).
      (translate
       (sdf-emit (second args) (format nil "(~A-~A)" p (glvec3 (first args)))))
      (t (error "unknown SDF node: ~S" head)))))

(defparameter *sdf-prelude* "
precision highp float;
uniform vec2  u_res;
uniform float u_time;
// The camera is computed in Lisp, not here. The shader renders; it does not
// decide.
uniform vec3  u_ro;
uniform vec3  u_ta;

float sdBox(vec3 p, vec3 b){ vec3 d = abs(p) - b;
  return length(max(d,0.0)) + min(max(d.x,max(d.y,d.z)),0.0); }
float sdTorus(vec3 p, vec2 t){ vec2 q = vec2(length(p.xz)-t.x, p.y);
  return length(q)-t.y; }
float smin(float a, float b, float k){
  float h = clamp(0.5+0.5*(b-a)/k, 0.0, 1.0);
  return mix(b,a,h) - k*h*(1.0-h); }
// A zero component means: do not repeat on this axis. mod(x, 0.0) is
// undefined and NaNs the whole distance field, which is not a subtle failure.
vec3 opRep(vec3 p, vec3 c){
  vec3 q = p;
  if(c.x > 0.0) q.x = mod(p.x+0.5*c.x, c.x) - 0.5*c.x;
  if(c.y > 0.0) q.y = mod(p.y+0.5*c.y, c.y) - 0.5*c.y;
  if(c.z > 0.0) q.z = mod(p.z+0.5*c.z, c.z) - 0.5*c.z;
  return q;
}
vec3 opTwist(vec3 p, float k){ float c=cos(k*p.y), s=sin(k*p.y);
  return vec3(c*p.x-s*p.z, p.y, s*p.x+c*p.z); }
")

(defparameter *sdf-body* "
vec3 normalAt(vec3 p){
  vec2 e = vec2(0.0015, 0.0);
  return normalize(vec3(map(p+e.xyy)-map(p-e.xyy),
                        map(p+e.yxy)-map(p-e.yxy),
                        map(p+e.yyx)-map(p-e.yyx)));
}

float shadow(vec3 ro, vec3 rd){
  float res = 1.0, t = 0.05;
  for(int i=0;i<24;i++){
    float h = map(ro+rd*t);
    res = min(res, 10.0*h/t);
    t += clamp(h, 0.02, 0.35);
    if(res < 0.005 || t > 12.0) break;
  }
  return clamp(res, 0.0, 1.0);
}

void main(){
  // Normalise by the SHORT side. Dividing by height on a tall phone screen
  // leaves a horizontal field of about a quarter unit, which reads as being
  // pressed against the geometry.
  vec2 uv = (gl_FragCoord.xy - 0.5*u_res) / min(u_res.x, u_res.y);

  vec3 ro = u_ro, ta = u_ta;
  vec3 f = normalize(ta-ro), r = normalize(cross(vec3(0,1,0), f)), u = cross(f, r);
  vec3 rd = normalize(uv.x*r + uv.y*u + 1.3*f);

  float t = 0.0; float d = 0.0; bool hit = false;
  for(int i=0;i<160;i++){
    vec3 p = ro + rd*t;
    d = map(p);
    if(d < 0.0008*t){ hit = true; break; }
    // 0.85 rather than a full step: smooth-union is only approximately a
    // distance field, and a full step through the approximation is what makes
    // edges crawl and thin geometry disappear.
    t += d*0.85;
    if(t > 45.0) break;
  }

  vec3 col = vec3(0.02,0.03,0.05) + 0.12*vec3(0.3,0.5,0.9)*(1.0-uv.y);
  if(hit){
    vec3 p = ro + rd*t;
    vec3 n = normalAt(p);
    vec3 ldir = normalize(vec3(0.7, 0.9, -0.4));
    float diff = max(dot(n, ldir), 0.0) * shadow(p+n*0.02, ldir);
    float ao   = clamp(0.6+0.4*n.y, 0.0, 1.0);
    float fres = pow(1.0-max(dot(n,-rd),0.0), 4.0);
    vec3 base  = 0.5 + 0.5*cos(vec3(0.0,2.1,4.2) + p.y*0.45 + u_time*0.3);
    col = base*(0.12*ao + 0.95*diff) + fres*vec3(0.5,0.7,1.0)*0.7;
    col = mix(col, vec3(0.02,0.03,0.05), 1.0-exp(-0.0016*t*t*t));
  }
  col = pow(clamp(col,0.0,1.0), vec3(0.4545));   // gamma
  gl_FragColor = vec4(col, 1.0);
}
")

(defun sdf-fragment-shader (scene)
  "Compile SCENE — a list — into a complete GLES 2.0 fragment shader.

Leaves *SDF-UNIFORMS* holding every symbol the scene used, which is the list the
render loop then drives."
  (setf *sdf-uniforms* '())
  (let ((map-body (sdf-emit scene "p")))     ; fills *sdf-uniforms* as it walks
    (concatenate 'string
                 *sdf-prelude*
                 (format nil "~{uniform float u_~A;~%~}"
                         (mapcar #'uniform-name *sdf-uniforms*))
                 (format nil "float map(vec3 p){ return ~A; }~%" map-body)
                 *sdf-body*)))

;;;; The world. Change this list, change the universe.
(defparameter *scene*
  '(union
    ;; A blob: sphere and torus melted together. BLEND is a symbol, so the
    ;; melting is a live value Lisp moves every frame, not a constant baked into
    ;; the shader.
    (smooth-union blend
     (sphere 1.05 :at (0 1.15 0))
     (torus 1.95 0.3 :at (0 1.15 0)))
    ;; A colonnade marching off in both directions, folded out of ONE box by a
    ;; modulo on x and z.
    ;;
    ;; The box is centred in its cell and the whole lattice is then shifted by
    ;; half a cell. Off-centre content is what produced the half-eaten pillars:
    ;; domain repetition returns the distance to the copy in the point's OWN
    ;; cell, so when a copy in the neighbouring cell is nearer the field
    ;; overestimates, rays overshoot, and surfaces are punched through.
    (translate (3.5 0 3.5)
     (repeat (7 0 7) (box (0.32 pillar-height 0.32) :at (0 1.6 0))))
    (plane -0.55)))

;;;; ── The simulation ───────────────────────────────────────────────────
;;;;
;;;; Everything the renderer needs per frame, decided here. The shader has no
;;;; clock of its own for these: stop calling this and the world stops.

(defun camera-at (seconds)
  "Camera origin and target. A lissajous drift rather than a plain orbit, so the
path is visibly something chosen rather than a circle."
  (let* ((a (* seconds 0.31))
         (radius (+ 8.0 (* 2.2 (sin (* seconds 0.23)))))
         (x (* radius (cos a)))
         (z (* radius (sin (* 1.31 a))))
         (y (+ 3.1 (* 1.6 (sin (* seconds 0.47))))))
    (values (list x y z)
            (list (* 0.6 (sin (* seconds 0.19)))
                  (+ 1.0 (* 0.35 (sin (* seconds 0.37))))
                  (* 0.6 (cos (* seconds 0.17)))))))

(defun scene-uniforms (seconds)
  "Live values for the symbols the scene used, as an alist of (symbol . value)."
  (list
   ;; The blob melts and re-separates.
   (cons 'blend (+ 0.5 (* 0.42 (sin (* seconds 0.6)))))
   ;; The colonnade breathes.
   (cons 'pillar-height (+ 2.8 (* 1.4 (sin (* seconds 0.41)))))))
