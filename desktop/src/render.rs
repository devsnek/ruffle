#![allow(clippy::invalid_ref)]

use glium::uniforms::{UniformValue, Uniforms};
use glium::{draw_parameters::DrawParameters, implement_vertex, uniform, Display, Frame, Surface};
use glutin::WindowedContext;
use lyon::tessellation::geometry_builder::{BuffersBuilder, VertexBuffers};
use lyon::{
    path::PathEvent, tessellation, tessellation::FillTessellator, tessellation::StrokeTessellator,
};
use ruffle_core::backend::render::swf::{self, FillStyle};
use ruffle_core::backend::render::{BitmapHandle, Color, Letterbox, RenderBackend, ShapeHandle, Transform};
use ruffle_core::shape_utils::{DrawCommand, DrawPath};
use swf::Twips;

pub struct GliumRenderBackend {
    display: Display,
    target: Option<Frame>,
    shader_program: glium::Program,
    gradient_shader_program: glium::Program,
    bitmap_shader_program: glium::Program,
    meshes: Vec<Mesh>,
    textures: Vec<(swf::CharacterId, Texture)>,
    viewport_width: f32,
    viewport_height: f32,
    view_matrix: [[f32; 4]; 4],
}

impl GliumRenderBackend {
    pub fn new(
        windowed_context: WindowedContext,
    ) -> Result<GliumRenderBackend, Box<dyn std::error::Error>> {
        let display = Display::from_gl_window(windowed_context)?;

        use glium::program::ProgramCreationInput;
        let shader_program = glium::Program::new(
            &display,
            ProgramCreationInput::SourceCode {
                vertex_shader: VERTEX_SHADER,
                fragment_shader: FRAGMENT_SHADER,
                geometry_shader: None,
                tessellation_control_shader: None,
                tessellation_evaluation_shader: None,
                transform_feedback_varyings: None,
                outputs_srgb: true,
                uses_point_size: false,
            },
        )?;

        let gradient_shader_program = glium::Program::new(
            &display,
            ProgramCreationInput::SourceCode {
                vertex_shader: TEXTURE_VERTEX_SHADER,
                fragment_shader: GRADIENT_FRAGMENT_SHADER,
                geometry_shader: None,
                tessellation_control_shader: None,
                tessellation_evaluation_shader: None,
                transform_feedback_varyings: None,
                outputs_srgb: true,
                uses_point_size: false,
            },
        )?;

        let bitmap_shader_program = glium::Program::new(
            &display,
            ProgramCreationInput::SourceCode {
                vertex_shader: TEXTURE_VERTEX_SHADER,
                fragment_shader: BITMAP_FRAGMENT_SHADER,
                geometry_shader: None,
                tessellation_control_shader: None,
                tessellation_evaluation_shader: None,
                transform_feedback_varyings: None,
                outputs_srgb: true,
                uses_point_size: false,
            },
        )?;

        let mut renderer = GliumRenderBackend {
            display,
            shader_program,
            gradient_shader_program,
            bitmap_shader_program,
            target: None,
            meshes: vec![],
            textures: vec![],
            viewport_width: 500.0,
            viewport_height: 500.0,
            view_matrix: [[0.0; 4]; 4],
        };
        renderer.build_matrices();
        Ok(renderer)
    }

    pub fn display(&self) -> &Display {
        &self.display
    }

    fn register_shape_internal(&mut self, shape: &swf::Shape) -> ShapeHandle {
        let handle = ShapeHandle(self.meshes.len());
        let paths = ruffle_core::shape_utils::swf_shape_to_paths(shape);

        use lyon::tessellation::{FillOptions, StrokeOptions};

        let mut mesh = Mesh { draws: vec![] };

        //let mut vertices: Vec<Vertex> = vec![];
        //let mut indices: Vec<u32> = vec![];

        let mut fill_tess = FillTessellator::new();
        let mut stroke_tess = StrokeTessellator::new();
        let mut lyon_mesh: VertexBuffers<_, u32> = VertexBuffers::new();

        fn flush_draw(
            draw: DrawType,
            mesh: &mut Mesh,
            lyon_mesh: &mut VertexBuffers<Vertex, u32>,
            display: &Display,
        ) {
            if lyon_mesh.vertices.is_empty() {
                return;
            }

            let vertex_buffer = glium::VertexBuffer::new(display, &lyon_mesh.vertices[..]).unwrap();

            let index_buffer = glium::IndexBuffer::new(
                display,
                glium::index::PrimitiveType::TrianglesList,
                &lyon_mesh.indices[..],
            )
            .unwrap();

            mesh.draws.push(Draw {
                draw_type: draw,
                vertex_buffer,
                index_buffer,
            });

            *lyon_mesh = VertexBuffers::new();
        }

        for path in paths {
            match path {
                DrawPath::Fill { style, commands } => match style {
                    FillStyle::Color(color) => {
                        let color = [
                            f32::from(color.r) / 255.0,
                            f32::from(color.g) / 255.0,
                            f32::from(color.b) / 255.0,
                            f32::from(color.a) / 255.0,
                        ];

                        let vertex_ctor = move |vertex: tessellation::FillVertex| Vertex {
                            position: [vertex.position.x, vertex.position.y],
                            color,
                        };
                        let mut buffers_builder = BuffersBuilder::new(&mut lyon_mesh, vertex_ctor);

                        if let Err(e) = fill_tess.tessellate_path(
                            ruffle_path_to_lyon_path(commands, true),
                            &FillOptions::even_odd(),
                            &mut buffers_builder,
                        ) {
                            println!("Failure");
                            log::error!("Tessellation failure: {:?}", e);
                            self.meshes.push(mesh);
                            return handle;
                        }
                    }
                    FillStyle::LinearGradient(gradient) => {
                        flush_draw(DrawType::Color, &mut mesh, &mut lyon_mesh, &self.display);

                        let vertex_ctor = move |vertex: tessellation::FillVertex| Vertex {
                            position: [vertex.position.x, vertex.position.y],
                            color: [1.0, 1.0, 1.0, 1.0],
                        };
                        let mut buffers_builder = BuffersBuilder::new(&mut lyon_mesh, vertex_ctor);

                        if let Err(e) = fill_tess.tessellate_path(
                            ruffle_path_to_lyon_path(commands, true),
                            &FillOptions::even_odd(),
                            &mut buffers_builder,
                        ) {
                            println!("Failure");
                            log::error!("Tessellation failure: {:?}", e);
                            self.meshes.push(mesh);
                            return handle;
                        }

                        let mut colors: Vec<[f32; 4]> = Vec::with_capacity(8);
                        let mut ratios: Vec<f32> = Vec::with_capacity(8);
                        for record in &gradient.records {
                            colors.push([
                                f32::from(record.color.r) / 255.0,
                                f32::from(record.color.g) / 255.0,
                                f32::from(record.color.b) / 255.0,
                                f32::from(record.color.a) / 255.0,
                            ]);
                            ratios.push(f32::from(record.ratio) / 255.0);
                        }

                        let uniforms = GradientUniforms {
                            gradient_type: 0,
                            ratios,
                            colors,
                            num_colors: gradient.records.len() as u32,
                            matrix: swf_to_gl_matrix(gradient.matrix.clone()),
                            repeat_mode: 0,
                            focal_point: 0.0,
                        };

                        flush_draw(
                            DrawType::Gradient(uniforms),
                            &mut mesh,
                            &mut lyon_mesh,
                            &self.display,
                        );
                    }
                    FillStyle::RadialGradient(gradient) => {
                        flush_draw(DrawType::Color, &mut mesh, &mut lyon_mesh, &self.display);

                        let vertex_ctor = move |vertex: tessellation::FillVertex| Vertex {
                            position: [vertex.position.x, vertex.position.y],
                            color: [1.0, 1.0, 1.0, 1.0],
                        };
                        let mut buffers_builder = BuffersBuilder::new(&mut lyon_mesh, vertex_ctor);

                        if let Err(e) = fill_tess.tessellate_path(
                            ruffle_path_to_lyon_path(commands, true),
                            &FillOptions::even_odd(),
                            &mut buffers_builder,
                        ) {
                            println!("Failure");
                            log::error!("Tessellation failure: {:?}", e);
                            self.meshes.push(mesh);
                            return handle;
                        }

                        let mut colors: Vec<[f32; 4]> = Vec::with_capacity(8);
                        let mut ratios: Vec<f32> = Vec::with_capacity(8);
                        for record in &gradient.records {
                            colors.push([
                                f32::from(record.color.r) / 255.0,
                                f32::from(record.color.g) / 255.0,
                                f32::from(record.color.b) / 255.0,
                                f32::from(record.color.a) / 255.0,
                            ]);
                            ratios.push(f32::from(record.ratio) / 255.0);
                        }

                        let uniforms = GradientUniforms {
                            gradient_type: 1,
                            ratios,
                            colors,
                            num_colors: gradient.records.len() as u32,
                            matrix: swf_to_gl_matrix(gradient.matrix.clone()),
                            repeat_mode: 0,
                            focal_point: 0.0,
                        };

                        flush_draw(
                            DrawType::Gradient(uniforms),
                            &mut mesh,
                            &mut lyon_mesh,
                            &self.display,
                        );
                    }
                    FillStyle::FocalGradient {
                        gradient,
                        focal_point,
                    } => {
                        flush_draw(DrawType::Color, &mut mesh, &mut lyon_mesh, &self.display);

                        let vertex_ctor = move |vertex: tessellation::FillVertex| Vertex {
                            position: [vertex.position.x, vertex.position.y],
                            color: [1.0, 1.0, 1.0, 1.0],
                        };
                        let mut buffers_builder = BuffersBuilder::new(&mut lyon_mesh, vertex_ctor);

                        if let Err(e) = fill_tess.tessellate_path(
                            ruffle_path_to_lyon_path(commands, true),
                            &FillOptions::even_odd(),
                            &mut buffers_builder,
                        ) {
                            println!("Failure");
                            log::error!("Tessellation failure: {:?}", e);
                            self.meshes.push(mesh);
                            return handle;
                        }

                        let mut colors: Vec<[f32; 4]> = Vec::with_capacity(8);
                        let mut ratios: Vec<f32> = Vec::with_capacity(8);
                        for record in &gradient.records {
                            colors.push([
                                f32::from(record.color.r) / 255.0,
                                f32::from(record.color.g) / 255.0,
                                f32::from(record.color.b) / 255.0,
                                f32::from(record.color.a) / 255.0,
                            ]);
                            ratios.push(f32::from(record.ratio) / 255.0);
                        }

                        let uniforms = GradientUniforms {
                            gradient_type: 1,
                            ratios,
                            colors,
                            num_colors: gradient.records.len() as u32,
                            matrix: swf_to_gl_matrix(gradient.matrix.clone()),
                            repeat_mode: 0,
                            focal_point: *focal_point,
                        };

                        flush_draw(
                            DrawType::Gradient(uniforms),
                            &mut mesh,
                            &mut lyon_mesh,
                            &self.display,
                        );
                    }
                    FillStyle::Bitmap { id, matrix, .. } => {
                        flush_draw(DrawType::Color, &mut mesh, &mut lyon_mesh, &self.display);

                        let vertex_ctor = move |vertex: tessellation::FillVertex| Vertex {
                            position: [vertex.position.x, vertex.position.y],
                            color: [1.0, 1.0, 1.0, 1.0],
                        };
                        let mut buffers_builder = BuffersBuilder::new(&mut lyon_mesh, vertex_ctor);

                        if let Err(e) = fill_tess.tessellate_path(
                            ruffle_path_to_lyon_path(commands, true),
                            &FillOptions::even_odd(),
                            &mut buffers_builder,
                        ) {
                            println!("Failure");
                            log::error!("Tessellation failure: {:?}", e);
                            self.meshes.push(mesh);
                            return handle;
                        }

                        let texture = &self
                            .textures
                            .iter()
                            .find(|(other_id, _tex)| *other_id == *id)
                            .unwrap()
                            .1;

                        let uniforms = BitmapUniforms {
                            matrix: swf_bitmap_to_gl_matrix(
                                matrix.clone(),
                                texture.width,
                                texture.height,
                            ),
                            id: *id,
                        };

                        flush_draw(
                            DrawType::Bitmap(uniforms),
                            &mut mesh,
                            &mut lyon_mesh,
                            &self.display,
                        );
                    }
                },
                DrawPath::Stroke {
                    style,
                    commands,
                    is_closed,
                } => {
                    let color = [
                        f32::from(style.color.r) / 255.0,
                        f32::from(style.color.g) / 255.0,
                        f32::from(style.color.b) / 255.0,
                        f32::from(style.color.a) / 255.0,
                    ];

                    let vertex_ctor = move |vertex: tessellation::StrokeVertex| Vertex {
                        position: [vertex.position.x, vertex.position.y],
                        color,
                    };
                    let mut buffers_builder = BuffersBuilder::new(&mut lyon_mesh, vertex_ctor);

                    // TODO(Herschel): 0 width indicates "hairline".
                    let width = if style.width.to_pixels() >= 1.0 {
                        style.width.to_pixels() as f32
                    } else {
                        1.0
                    };

                    let mut options = StrokeOptions::default()
                        .with_line_width(width)
                        .with_line_join(match style.join_style {
                            swf::LineJoinStyle::Round => tessellation::LineJoin::Round,
                            swf::LineJoinStyle::Bevel => tessellation::LineJoin::Bevel,
                            swf::LineJoinStyle::Miter(_) => tessellation::LineJoin::MiterClip,
                        })
                        .with_start_cap(match style.start_cap {
                            swf::LineCapStyle::None => tessellation::LineCap::Butt,
                            swf::LineCapStyle::Round => tessellation::LineCap::Round,
                            swf::LineCapStyle::Square => tessellation::LineCap::Square,
                        })
                        .with_end_cap(match style.end_cap {
                            swf::LineCapStyle::None => tessellation::LineCap::Butt,
                            swf::LineCapStyle::Round => tessellation::LineCap::Round,
                            swf::LineCapStyle::Square => tessellation::LineCap::Square,
                        });

                    if let swf::LineJoinStyle::Miter(limit) = style.join_style {
                        options = options.with_miter_limit(limit);
                    }

                    if let Err(e) = stroke_tess.tessellate_path(
                        ruffle_path_to_lyon_path(commands, is_closed),
                        &options,
                        &mut buffers_builder,
                    ) {
                        log::error!("Tessellation failure: {:?}", e);
                        self.meshes.push(mesh);
                        return handle;
                    }
                }
            }
        }

        flush_draw(DrawType::Color, &mut mesh, &mut lyon_mesh, &self.display);

        self.meshes.push(mesh);

        handle
    }

    fn build_matrices(&mut self) {
        self.view_matrix = [
            [1.0 / (self.viewport_width as f32 / 2.0), 0.0, 0.0, 0.0],
            [0.0, -1.0 / (self.viewport_height as f32 / 2.0), 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0, 1.0],
        ];
    }
}

impl RenderBackend for GliumRenderBackend {
    fn set_viewport_dimensions(&mut self, width: u32, height: u32) {
        self.viewport_width = width as f32;
        self.viewport_height = height as f32;
        self.build_matrices();
    }

    fn register_shape(&mut self, shape: &swf::Shape) -> ShapeHandle {
        self.register_shape_internal(shape)
    }

    fn register_glyph_shape(&mut self, glyph: &swf::Glyph) -> ShapeHandle {
        let shape = swf::Shape {
            version: 2,
            id: 0,
            shape_bounds: Default::default(),
            edge_bounds: Default::default(),
            has_fill_winding_rule: false,
            has_non_scaling_strokes: false,
            has_scaling_strokes: true,
            styles: swf::ShapeStyles {
                fill_styles: vec![FillStyle::Color(Color {
                    r: 255,
                    g: 255,
                    b: 255,
                    a: 255,
                })],
                line_styles: vec![],
            },
            shape: glyph.shape_records.clone(),
        };
        self.register_shape_internal(&shape)
    }

    fn register_bitmap_jpeg(
        &mut self,
        id: swf::CharacterId,
        data: &[u8],
        jpeg_tables: &[u8],
    ) -> BitmapHandle {
        if !jpeg_tables.is_empty() {
            let mut full_jpeg = jpeg_tables[..jpeg_tables.len() - 2].to_vec();
            full_jpeg.extend_from_slice(&data[2..]);

            self.register_bitmap_jpeg_2(id, &full_jpeg[..])
        } else {
            self.register_bitmap_jpeg_2(id, &data[..])
        }
    }

    fn register_bitmap_jpeg_2(&mut self, id: swf::CharacterId, data: &[u8]) -> BitmapHandle {
        let data = ruffle_core::backend::render::remove_invalid_jpeg_data(data);

        let mut decoder = jpeg_decoder::Decoder::new(&data[..]);
        decoder.read_info().unwrap();
        let metadata = decoder.info().unwrap();
        let decoded_data = decoder.decode().expect("failed to decode image");
        let image = glium::texture::RawImage2d::from_raw_rgb(
            decoded_data,
            (metadata.width.into(), metadata.height.into()),
        );

        let texture = glium::texture::Texture2d::new(&self.display, image).unwrap();

        let handle = BitmapHandle(self.textures.len());
        self.textures.push((
            id,
            Texture {
                texture,
                width: metadata.width.into(),
                height: metadata.height.into(),
            },
        ));

        handle
    }

    fn register_bitmap_jpeg_3(
        &mut self,
        id: swf::CharacterId,
        jpeg_data: &[u8],
        alpha_data: &[u8],
    ) -> BitmapHandle {
        let (width, height, rgba) =
            ruffle_core::backend::render::define_bits_jpeg_to_rgba(jpeg_data, alpha_data)
                .expect("Error decoding DefineBitsJPEG3");

        let image = glium::texture::RawImage2d::from_raw_rgba(rgba, (width, height));
        let texture = glium::texture::Texture2d::new(&self.display, image).unwrap();
        let handle = BitmapHandle(self.textures.len());
        self.textures.push((
            id,
            Texture {
                texture,
                width,
                height,
            },
        ));

        handle
    }

    fn register_bitmap_png(&mut self, swf_tag: &swf::DefineBitsLossless) -> BitmapHandle {
        let decoded_data = ruffle_core::backend::render::define_bits_lossless_to_rgba(swf_tag)
            .expect("Error decoding DefineBitsLossless");

        let image = glium::texture::RawImage2d::from_raw_rgba(
            decoded_data,
            (swf_tag.width.into(), swf_tag.height.into()),
        );

        let texture = glium::texture::Texture2d::new(&self.display, image).unwrap();

        let handle = BitmapHandle(self.textures.len());
        self.textures.push((
            swf_tag.id,
            Texture {
                texture,
                width: swf_tag.width.into(),
                height: swf_tag.height.into(),
            },
        ));

        handle
    }

    fn begin_frame(&mut self) {
        assert!(self.target.is_none());
        self.target = Some(self.display.draw());
    }

    fn end_frame(&mut self) {
        assert!(self.target.is_some());

        let target = self.target.take().unwrap();
        target.finish().unwrap();
    }

    fn clear(&mut self, color: Color) {
        let target = self.target.as_mut().unwrap();
        target.clear_color_srgb(
            f32::from(color.r) / 255.0,
            f32::from(color.g) / 255.0,
            f32::from(color.b) / 255.0,
            f32::from(color.a) / 255.0,
        );
    }

    fn render_shape(&mut self, shape: ShapeHandle, transform: &Transform) {
        let target = self.target.as_mut().unwrap();

        let mesh = &self.meshes[shape.0];

        let world_matrix = [
            [transform.matrix.a, transform.matrix.b, 0.0, 0.0],
            [transform.matrix.c, transform.matrix.d, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [
                transform.matrix.tx / 20.0,
                transform.matrix.ty / 20.0,
                0.0,
                1.0,
            ],
        ];

        let mult_color = [
            transform.color_transform.r_mult,
            transform.color_transform.g_mult,
            transform.color_transform.b_mult,
            transform.color_transform.a_mult,
        ];

        let add_color = [
            transform.color_transform.r_add,
            transform.color_transform.g_add,
            transform.color_transform.b_add,
            transform.color_transform.a_add,
        ];

        let draw_parameters = DrawParameters {
            blend: glium::Blend::alpha_blending(),
            ..Default::default()
        };

        for draw in &mesh.draws {
            match &draw.draw_type {
                DrawType::Color => {
                    target
                        .draw(
                            &draw.vertex_buffer,
                            &draw.index_buffer,
                            &self.shader_program,
                            &uniform! { view_matrix: self.view_matrix, world_matrix: world_matrix, mult_color: mult_color, add_color: add_color },
                            &draw_parameters
                        )
                        .unwrap();
                }
                DrawType::Gradient(gradient_uniforms) => {
                    let uniforms = GradientUniformsFull {
                        view_matrix: self.view_matrix,
                        world_matrix,
                        mult_color,
                        add_color,
                        gradient: gradient_uniforms.clone(),
                    };

                    target
                        .draw(
                            &draw.vertex_buffer,
                            &draw.index_buffer,
                            &self.gradient_shader_program,
                            &uniforms,
                            &draw_parameters,
                        )
                        .unwrap();
                }
                DrawType::Bitmap(bitmap_uniforms) => {
                    let texture = &self
                        .textures
                        .iter()
                        .find(|(id, _tex)| *id == bitmap_uniforms.id)
                        .unwrap()
                        .1;

                    let uniforms = BitmapUniformsFull {
                        view_matrix: self.view_matrix,
                        world_matrix,
                        mult_color,
                        add_color,
                        matrix: bitmap_uniforms.matrix,
                        texture: &texture.texture,
                    };

                    target
                        .draw(
                            &draw.vertex_buffer,
                            &draw.index_buffer,
                            &self.bitmap_shader_program,
                            &uniforms,
                            &draw_parameters,
                        )
                        .unwrap();
                }
            }
        }
    }

    fn draw_pause_overlay(&mut self) {}

    fn draw_letterbox(&mut self, letterbox: Letterbox) {
        let target = self.target.as_mut().unwrap();
        let black = Some((0.0, 0.0, 0.0, 1.0));
        match letterbox {
            Letterbox::None => (),
            Letterbox::Letterbox(margin_height) => {
                target.clear(
                    Some(&glium::Rect {
                        left: 0,
                        bottom: 0,
                        width: self.viewport_width as u32,
                        height: margin_height as u32,
                    }),
                    black,
                    true,
                    None,
                    None,
                );
                target.clear(
                    Some(&glium::Rect {
                        left: 0,
                        bottom: (self.viewport_height - margin_height) as u32,
                        width: self.viewport_width as u32,
                        height: margin_height as u32,
                    }),
                    black,
                    true,
                    None,
                    None,
                );
            }
            Letterbox::Pillarbox(margin_width) => {
                target.clear(
                    Some(&glium::Rect {
                        left: 0,
                        bottom: 0,
                        width: margin_width as u32,
                        height: self.viewport_height as u32,
                    }),
                    black,
                    true,
                    None,
                    None,
                );
                target.clear(
                    Some(&glium::Rect {
                        left: (self.viewport_width - margin_width) as u32,
                        bottom: 0,
                        width: margin_width as u32,
                        height: self.viewport_height as u32,
                    }),
                    black,
                    true,
                    None,
                    None,
                );
            }
        }
    }
}

struct Texture {
    width: u32,
    height: u32,
    texture: glium::Texture2d,
}

#[derive(Copy, Clone, Debug)]
struct Vertex {
    position: [f32; 2],
    color: [f32; 4],
}

implement_vertex!(Vertex, position, color);

#[derive(Clone, Debug)]
struct GradientUniforms {
    matrix: [[f32; 3]; 3],
    gradient_type: i32,
    ratios: Vec<f32>,
    colors: Vec<[f32; 4]>,
    num_colors: u32,
    repeat_mode: i32,
    focal_point: f32,
}

impl Uniforms for GradientUniforms {
    fn visit_values<'a, F: FnMut(&str, UniformValue<'a>)>(&'a self, mut visit: F) {
        visit("u_matrix", UniformValue::Mat3(self.matrix));
        visit(
            "u_gradient_type",
            UniformValue::SignedInt(self.gradient_type),
        );
        for i in 0..self.num_colors as usize {
            visit(
                &format!("u_ratios[{}]", i)[..],
                UniformValue::Float(self.ratios[i]),
            );
            visit(
                &format!("u_colors[{}]", i)[..],
                UniformValue::Vec4(self.colors[i]),
            );
        }
        visit("u_num_colors", UniformValue::UnsignedInt(self.num_colors));
        visit("u_repeat_mode", UniformValue::SignedInt(self.repeat_mode));
        visit("u_focal_point", UniformValue::Float(self.focal_point));
    }
}

#[derive(Clone, Debug)]
struct GradientUniformsFull {
    world_matrix: [[f32; 4]; 4],
    view_matrix: [[f32; 4]; 4],
    mult_color: [f32; 4],
    add_color: [f32; 4],
    gradient: GradientUniforms,
}

impl Uniforms for GradientUniformsFull {
    fn visit_values<'a, F: FnMut(&str, UniformValue<'a>)>(&'a self, mut visit: F) {
        visit("world_matrix", UniformValue::Mat4(self.world_matrix));
        visit("view_matrix", UniformValue::Mat4(self.view_matrix));
        visit("mult_color", UniformValue::Vec4(self.mult_color));
        visit("add_color", UniformValue::Vec4(self.add_color));
        self.gradient.visit_values(visit);
    }
}

#[derive(Clone, Debug)]
struct BitmapUniforms {
    matrix: [[f32; 3]; 3],
    id: swf::CharacterId,
}

impl Uniforms for BitmapUniforms {
    fn visit_values<'a, F: FnMut(&str, UniformValue<'a>)>(&'a self, mut visit: F) {
        visit("u_matrix", UniformValue::Mat3(self.matrix));
    }
}

#[derive(Clone, Debug)]
struct BitmapUniformsFull<'a> {
    world_matrix: [[f32; 4]; 4],
    view_matrix: [[f32; 4]; 4],
    mult_color: [f32; 4],
    add_color: [f32; 4],
    matrix: [[f32; 3]; 3],
    texture: &'a glium::Texture2d,
}

impl<'a> Uniforms for BitmapUniformsFull<'a> {
    fn visit_values<'v, F: FnMut(&str, UniformValue<'v>)>(&'v self, mut visit: F) {
        visit("world_matrix", UniformValue::Mat4(self.world_matrix));
        visit("view_matrix", UniformValue::Mat4(self.view_matrix));
        visit("mult_color", UniformValue::Vec4(self.mult_color));
        visit("add_color", UniformValue::Vec4(self.add_color));
        visit("u_matrix", UniformValue::Mat3(self.matrix));
        visit("u_texture", UniformValue::Texture2d(self.texture, None));
    }
}

const VERTEX_SHADER: &str = r#"
    #version 140

    uniform mat4 view_matrix;
    uniform mat4 world_matrix;
    uniform vec4 mult_color;
    uniform vec4 add_color;
    
    in vec2 position;
    in vec4 color;
    out vec4 frag_color;

    void main() {
        frag_color = color * mult_color + add_color;
        gl_Position = view_matrix * world_matrix * vec4(position, 0.0, 1.0);
    }
"#;

const FRAGMENT_SHADER: &str = r#"
    #version 140
    in vec4 frag_color;
    out vec4 out_color;
    void main() {
        out_color = frag_color;
    }
"#;

const TEXTURE_VERTEX_SHADER: &str = r#"
    #version 140

    uniform mat4 view_matrix;
    uniform mat4 world_matrix;

    uniform mat3 u_matrix;

    in vec2 position;
    in vec4 color;
    out vec2 frag_uv;

    void main() {
        frag_uv = vec2(u_matrix * vec3(position, 1.0));
        gl_Position = view_matrix * world_matrix * vec4(position, 0.0, 1.0);
    }
"#;

const GRADIENT_FRAGMENT_SHADER: &str = r#"
#version 140
    uniform vec4 mult_color;
    uniform vec4 add_color;

    uniform int u_gradient_type;
    uniform float u_ratios[8];
    uniform vec4 u_colors[8];
    uniform uint u_num_colors;
    uniform int u_repeat_mode;
    uniform float u_focal_point;

    in vec2 frag_uv;
    out vec4 out_color;

    void main() {
        vec4 color;
        int last = int(int(u_num_colors) - 1);
        float t;
        if( u_gradient_type == 0 )
        {
            t = frag_uv.x;
        }
        else if( u_gradient_type == 1 )
        {
            t = length(frag_uv * 2.0 - 1.0);
        }
        else if( u_gradient_type == 2 )
        {
            vec2 uv = frag_uv * 2.0 - 1.0;
            vec2 d = vec2(u_focal_point, 0.0) - uv;
            float l = length(d);
            d /= l;
            t = l / (sqrt(1.0 -  u_focal_point*u_focal_point*d.y*d.y) + u_focal_point*d.x);
        }
        if( u_repeat_mode == 0 )
        {
            // Clamp
            t = clamp(t, 0.0, 1.0);
        }
        else if( u_repeat_mode == 1 )
        {
            // Repeat
            t = fract(t);
        }
        else
        {
            // Mirror
            if( t < 0.0 )
            {
                t = -t;
            }
            if( (int(t)&1) == 0 ) {
                t = fract(t);
            } else {
                t = 1.0 - fract(t);
            }
        }
        int i = 0;
        int j = 1;
        while( t > u_ratios[j] )
        {
            i = j;
            j++;
        }
        float a = (t - u_ratios[i]) / (u_ratios[j] - u_ratios[i]);
        color = mix(u_colors[i], u_colors[j], a);            
        out_color = mult_color * color + add_color;
    }
"#;

const BITMAP_FRAGMENT_SHADER: &str = r#"
#version 140
    uniform vec4 mult_color;
    uniform vec4 add_color;

    in vec2 frag_uv;
    out vec4 out_color;

    uniform sampler2D u_texture;

    void main() {
        vec4 color = texture(u_texture, frag_uv);
        out_color = mult_color * color + add_color;
    }
"#;

struct Mesh {
    draws: Vec<Draw>,
}

struct Draw {
    draw_type: DrawType,
    vertex_buffer: glium::VertexBuffer<Vertex>,
    index_buffer: glium::IndexBuffer<u32>,
}

enum DrawType {
    Color,
    Gradient(GradientUniforms),
    Bitmap(BitmapUniforms),
}

fn point(x: Twips, y: Twips) -> lyon::math::Point {
    lyon::math::Point::new(x.to_pixels() as f32, y.to_pixels() as f32)
}

fn ruffle_path_to_lyon_path(
    commands: Vec<DrawCommand>,
    mut is_closed: bool,
) -> impl Iterator<Item = PathEvent> {
    use lyon::geom::{LineSegment, QuadraticBezierSegment};

    let mut cur = lyon::math::Point::new(0.0, 0.0);
    let mut i = commands.into_iter();
    std::iter::from_fn(move || match i.next() {
        Some(DrawCommand::MoveTo { x, y }) => {
            cur = point(x, y);
            Some(PathEvent::MoveTo(cur))
        }
        Some(DrawCommand::LineTo { x, y }) => {
            let next = point(x, y);
            let cmd = PathEvent::Line(LineSegment {
                from: cur,
                to: next,
            });
            cur = next;
            Some(cmd)
        }
        Some(DrawCommand::CurveTo { x1, y1, x2, y2 }) => {
            let next = point(x2, y2);
            let cmd = PathEvent::Quadratic(QuadraticBezierSegment {
                from: cur,
                ctrl: point(x1, y1),
                to: next,
            });
            cur = next;
            Some(cmd)
        }
        None => {
            if is_closed {
                is_closed = false;
                Some(PathEvent::Close(LineSegment { from: cur, to: cur }))
            } else {
                None
            }
        }
    })
}

#[allow(clippy::many_single_char_names)]
fn swf_to_gl_matrix(m: swf::Matrix) -> [[f32; 3]; 3] {
    let tx = m.translate_x.get() as f32;
    let ty = m.translate_y.get() as f32;
    let det = m.scale_x * m.scale_y - m.rotate_skew_1 * m.rotate_skew_0;
    let mut a = m.scale_y / det;
    let mut b = -m.rotate_skew_1 / det;
    let mut c = -(tx * m.scale_y - m.rotate_skew_1 * ty) / det;
    let mut d = -m.rotate_skew_0 / det;
    let mut e = m.scale_x / det;
    let mut f = (tx * m.rotate_skew_0 - m.scale_x * ty) / det;

    a *= 20.0 / 32768.0;
    b *= 20.0 / 32768.0;
    d *= 20.0 / 32768.0;
    e *= 20.0 / 32768.0;

    c /= 32768.0;
    f /= 32768.0;
    c += 0.5;
    f += 0.5;
    [[a, d, 0.0], [b, e, 0.0], [c, f, 1.0]]
}

#[allow(clippy::many_single_char_names)]
fn swf_bitmap_to_gl_matrix(m: swf::Matrix, bitmap_width: u32, bitmap_height: u32) -> [[f32; 3]; 3] {
    let bitmap_width = bitmap_width as f32;
    let bitmap_height = bitmap_height as f32;

    let tx = m.translate_x.get() as f32;
    let ty = m.translate_y.get() as f32;
    let det = m.scale_x * m.scale_y - m.rotate_skew_1 * m.rotate_skew_0;
    let mut a = m.scale_y / det;
    let mut b = -m.rotate_skew_1 / det;
    let mut c = -(tx * m.scale_y - m.rotate_skew_1 * ty) / det;
    let mut d = -m.rotate_skew_0 / det;
    let mut e = m.scale_x / det;
    let mut f = (tx * m.rotate_skew_0 - m.scale_x * ty) / det;

    a *= 20.0 / bitmap_width;
    b *= 20.0 / bitmap_width;
    d *= 20.0 / bitmap_height;
    e *= 20.0 / bitmap_height;

    c /= bitmap_width;
    f /= bitmap_height;

    [[a, d, 0.0], [b, e, 0.0], [c, f, 1.0]]
}
