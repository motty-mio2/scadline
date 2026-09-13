use crate::domain::Mesh;
use eframe::glow;
use glow::HasContext as _;

pub(super) struct SceneRenderer {
    program: glow::Program,
    vertex_array: glow::VertexArray,
    vertex_buffer: glow::Buffer,
    framebuffer: glow::Framebuffer,
    color_buffer: glow::Renderbuffer,
    depth_buffer: glow::Renderbuffer,
    render_size: [i32; 2],
    cpu_vertices: Vec<f32>,
    vertex_count: i32,
    dirty: bool,
}

pub(super) struct ScenePaintParams {
    pub(super) angles: [f32; 2],
    pub(super) zoom: f32,
    pub(super) pan: [f32; 2],
    pub(super) aspect: f32,
    pub(super) show_edges: bool,
    pub(super) viewport: [i32; 4],
    pub(super) destination: Option<glow::Framebuffer>,
}

impl SceneRenderer {
    pub(super) fn new(gl: &glow::Context) -> Self {
        unsafe {
            let program = gl.create_program().expect("OpenGL program creation failed");
            let shader_sources = [
                (
                    glow::VERTEX_SHADER,
                    r#"#version 330
                    layout(location = 0) in vec3 a_position;
                    layout(location = 1) in vec3 a_normal;
                    uniform vec2 u_angles;
                    uniform vec2 u_scale;
                    uniform vec2 u_pan;
                    out vec3 v_normal;

                    vec3 rotate_model(vec3 p) {
                        float sy = sin(u_angles.x), cy = cos(u_angles.x);
                        float sp = sin(u_angles.y), cp = cos(u_angles.y);
                        vec3 y = vec3(cy*p.x + sy*p.z, p.y, -sy*p.x + cy*p.z);
                        return vec3(y.x, cp*y.y - sp*y.z, sp*y.y + cp*y.z);
                    }

                    void main() {
                        vec3 p = rotate_model(a_position);
                        v_normal = rotate_model(a_normal);
                        gl_Position = vec4(p.xy * u_scale + u_pan, -p.z * 0.25, 1.0);
                    }"#,
                ),
                (
                    glow::FRAGMENT_SHADER,
                    r#"#version 330
                    in vec3 v_normal;
                    uniform bool u_wire;
                    out vec4 out_color;
                    void main() {
                        if (u_wire) {
                            out_color = vec4(0.035, 0.045, 0.055, 0.72);
                            return;
                        }
                        vec3 light = normalize(vec3(-0.35, 0.55, 1.0));
                        float shade = 0.30 + 0.70 * abs(dot(normalize(v_normal), light));
                        out_color = vec4(vec3(0.22, 0.62, 0.78) * shade + vec3(0.06), 1.0);
                    }"#,
                ),
            ];
            let mut shaders = Vec::new();
            for (kind, source) in shader_sources {
                let shader = gl
                    .create_shader(kind)
                    .expect("OpenGL shader creation failed");
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                assert!(
                    gl.get_shader_compile_status(shader),
                    "OpenGL shader error: {}",
                    gl.get_shader_info_log(shader)
                );
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.link_program(program);
            assert!(
                gl.get_program_link_status(program),
                "OpenGL link error: {}",
                gl.get_program_info_log(program)
            );
            for shader in shaders {
                gl.detach_shader(program, shader);
                gl.delete_shader(shader);
            }

            let vertex_array = gl
                .create_vertex_array()
                .expect("OpenGL VAO creation failed");
            let vertex_buffer = gl.create_buffer().expect("OpenGL buffer creation failed");
            let framebuffer = gl
                .create_framebuffer()
                .expect("OpenGL framebuffer creation failed");
            let color_buffer = gl
                .create_renderbuffer()
                .expect("OpenGL color buffer creation failed");
            let depth_buffer = gl
                .create_renderbuffer()
                .expect("OpenGL depth buffer creation failed");
            gl.bind_vertex_array(Some(vertex_array));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vertex_buffer));
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, 24, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 3, glow::FLOAT, false, 24, 12);

            Self {
                program,
                vertex_array,
                vertex_buffer,
                framebuffer,
                color_buffer,
                depth_buffer,
                render_size: [0, 0],
                cpu_vertices: Vec::new(),
                vertex_count: 0,
                dirty: false,
            }
        }
    }

    pub(super) fn set_mesh(&mut self, mesh: &Mesh) {
        self.cpu_vertices.clear();
        self.cpu_vertices.reserve(mesh.triangles.len() * 18);
        for triangle in &mesh.triangles {
            let a = mesh.vertices[triangle[0]]
                .sub(mesh.center)
                .mul(1.0 / mesh.radius);
            let b = mesh.vertices[triangle[1]]
                .sub(mesh.center)
                .mul(1.0 / mesh.radius);
            let c = mesh.vertices[triangle[2]]
                .sub(mesh.center)
                .mul(1.0 / mesh.radius);
            let normal = b.sub(a).cross(c.sub(a)).normalized();
            for vertex in [a, b, c] {
                self.cpu_vertices.extend_from_slice(&[
                    vertex.x, vertex.y, vertex.z, normal.x, normal.y, normal.z,
                ]);
            }
        }
        self.vertex_count = (self.cpu_vertices.len() / 6) as i32;
        self.dirty = true;
    }

    pub(super) fn paint(&mut self, gl: &glow::Context, params: ScenePaintParams) {
        let ScenePaintParams {
            angles,
            zoom,
            pan,
            aspect,
            show_edges,
            viewport,
            destination,
        } = params;
        unsafe {
            let width = viewport[2].max(1);
            let height = viewport[3].max(1);
            self.ensure_render_target(gl, width, height);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.framebuffer));
            gl.viewport(0, 0, width, height);
            gl.disable(glow::SCISSOR_TEST);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.disable(glow::BLEND);
            gl.clear_color(24.0 / 255.0, 27.0 / 255.0, 32.0 / 255.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vertex_array));
            if self.dirty {
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vertex_buffer));
                gl.buffer_data_u8_slice(
                    glow::ARRAY_BUFFER,
                    bytemuck::cast_slice(&self.cpu_vertices),
                    glow::STATIC_DRAW,
                );
                self.dirty = false;
            }

            let scale = if aspect >= 1.0 {
                [0.84 * zoom / aspect, 0.84 * zoom]
            } else {
                [0.84 * zoom, 0.84 * zoom * aspect]
            };
            gl.uniform_2_f32(
                gl.get_uniform_location(self.program, "u_angles").as_ref(),
                angles[0],
                angles[1],
            );
            gl.uniform_2_f32(
                gl.get_uniform_location(self.program, "u_scale").as_ref(),
                scale[0],
                scale[1],
            );
            gl.uniform_2_f32(
                gl.get_uniform_location(self.program, "u_pan").as_ref(),
                pan[0],
                pan[1],
            );
            gl.uniform_1_i32(gl.get_uniform_location(self.program, "u_wire").as_ref(), 0);

            gl.enable(glow::POLYGON_OFFSET_FILL);
            gl.polygon_offset(1.0, 1.0);
            gl.draw_arrays(glow::TRIANGLES, 0, self.vertex_count);
            gl.disable(glow::POLYGON_OFFSET_FILL);

            if show_edges {
                gl.uniform_1_i32(gl.get_uniform_location(self.program, "u_wire").as_ref(), 1);
                gl.polygon_mode(glow::FRONT_AND_BACK, glow::LINE);
                gl.line_width(1.0);
                gl.draw_arrays(glow::TRIANGLES, 0, self.vertex_count);
                gl.polygon_mode(glow::FRONT_AND_BACK, glow::FILL);
            }
            gl.disable(glow::DEPTH_TEST);

            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(self.framebuffer));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, destination);
            gl.blit_framebuffer(
                0,
                0,
                width,
                height,
                viewport[0],
                viewport[1],
                viewport[0] + width,
                viewport[1] + height,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, destination);
        }
    }

    fn ensure_render_target(&mut self, gl: &glow::Context, width: i32, height: i32) {
        if self.render_size == [width, height] {
            return;
        }
        self.render_size = [width, height];

        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.framebuffer));
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(self.color_buffer));
            gl.renderbuffer_storage(glow::RENDERBUFFER, glow::RGBA8, width, height);
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::RENDERBUFFER,
                Some(self.color_buffer),
            );
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(self.depth_buffer));
            gl.renderbuffer_storage(glow::RENDERBUFFER, glow::DEPTH_COMPONENT24, width, height);
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::DEPTH_ATTACHMENT,
                glow::RENDERBUFFER,
                Some(self.depth_buffer),
            );
            assert_eq!(
                gl.check_framebuffer_status(glow::FRAMEBUFFER),
                glow::FRAMEBUFFER_COMPLETE,
                "OpenGL framebuffer is incomplete"
            );
        }
    }

    pub(super) fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vertex_array);
            gl.delete_buffer(self.vertex_buffer);
            gl.delete_framebuffer(self.framebuffer);
            gl.delete_renderbuffer(self.color_buffer);
            gl.delete_renderbuffer(self.depth_buffer);
        }
    }
}
