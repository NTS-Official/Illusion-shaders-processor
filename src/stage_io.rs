//! Fragment stage input padding.
//!
//! D3D12 links the vertex and fragment stages by *signature register and component mask*, not by
//! semantic name: for every fragment input the runtime finds the vertex output with the same
//! `LOC<n>` semantic and requires the input's mask to be a subset of the output's mask in the same
//! register. Registers are assigned while the compiler packs the interface in declaration order,
//! so a fragment stage that consumes only part of the vertex interface packs its inputs
//! differently and `CreateGraphicsPipelineState` fails with `0x80070057` (`E_INVALIDARG`) on the
//! "hardware register component mask" check.
//!
//! The pass pads the fragment entry point with unused inputs in the vertex stage's declaration
//! order so both stages pack the interface identically. Each padded input is read by a `discard`
//! guard: an input the shader never reads is dropped from the compiled signature, and the packing
//! it was added for goes away with it.

use naga::{
    BinaryOperator, Binding, Block, Expression, FunctionArgument, Handle, Interpolation, Literal,
    Module, Range, Scalar, ScalarKind, ShaderStage, Span, Statement, Type, TypeInner, VectorSize,
};

/// Component count and scalar kind of a stage interface slot.
type Shape = (u8, ScalarKind);

/// One vertex stage output, in the order the vertex shader declares it.
///
/// The order carries the register packing, so the fragment stage reproduces the list as given
/// instead of sorting it by location.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageOutput {
    pub location: u32,
    /// HLSL spelling of the type (`float`, `float2`, `int3`, `uint4`, ...).
    pub hlsl_type: String,
    /// `Perspective` / `Linear` / `Flat`; omitted when the manifest does not say.
    #[serde(default)]
    pub interpolation: Option<String>,
}

/// Pads the fragment entry point's inputs to the vertex interface.
///
/// Returns the number of padded inputs; a module whose inputs already match the vertex interface
/// (every location present, in order) is left untouched and reports `0`. Errors describe why the
/// interface cannot be reproduced, which the caller reports instead of emitting a pipeline that
/// the runtime would reject.
pub fn pad_fragment_inputs(
    module: &mut Module,
    entry_name: &str,
    outputs: &[StageOutput],
) -> Result<usize, String> {
    if outputs.is_empty() {
        return Ok(0);
    }
    let entry_index = module
        .entry_points
        .iter()
        .position(|entry| entry.name == entry_name && entry.stage == ShaderStage::Fragment)
        .ok_or_else(|| format!("no fragment entry point `{entry_name}` to pad"))?;

    let mut builtins: Vec<FunctionArgument> = Vec::new();
    let mut located: Vec<(u32, FunctionArgument)> = Vec::new();
    for argument in &module.entry_points[entry_index].function.arguments {
        match &argument.binding {
            Some(Binding::BuiltIn(_)) => builtins.push(argument.clone()),
            Some(Binding::Location { location, .. }) => located.push((*location, argument.clone())),
            None => return Err("fragment input without a binding".to_owned()),
        }
    }
    for (location, _) in &located {
        if !outputs.iter().any(|output| output.location == *location) {
            return Err(format!("fragment input LOC{location} has no vertex output"));
        }
    }

    let mut padded = 0usize;
    let mut padded_arguments: Vec<(usize, Literal, u8)> = Vec::new();
    let mut rebuilt: Vec<FunctionArgument> = Vec::with_capacity(outputs.len());
    for output in outputs {
        let (components, kind) = parse_hlsl_type(&output.hlsl_type)
            .ok_or_else(|| format!("unsupported vertex output type `{}`", output.hlsl_type))?;
        match located
            .iter()
            .find(|(location, _)| *location == output.location)
        {
            Some((_, argument)) => {
                let shape = type_shape(module, argument.ty).ok_or_else(|| {
                    format!(
                        "fragment input LOC{} is not a scalar or vector",
                        output.location
                    )
                })?;
                if shape != (components, kind) {
                    return Err(format!(
                        "fragment input LOC{} is {}{kind:?}, the vertex output is {}",
                        output.location,
                        components_label(shape.0),
                        output.hlsl_type
                    ));
                }
                rebuilt.push(argument.clone());
            }
            None => {
                let ty = intern_type(module, components, kind);
                padded_arguments.push((
                    builtins.len() + rebuilt.len(),
                    literal_for(kind),
                    components,
                ));
                rebuilt.push(FunctionArgument {
                    name: Some(format!("dxr_pad{}", output.location)),
                    ty,
                    binding: Some(Binding::Location {
                        location: output.location,
                        interpolation: parse_interpolation(output.interpolation.as_deref(), kind),
                        sampling: None,
                        blend_src: None,
                        per_primitive: false,
                    }),
                });
                padded += 1;
            }
        }
    }

    let order_matches = located
        .iter()
        .map(|(location, _)| *location)
        .eq(outputs.iter().map(|output| output.location));
    if padded == 0 && order_matches {
        return Ok(0);
    }

    // `arguments` order decides the packing, and the body refers to arguments by index, so the
    // remap is computed before the new order is installed.
    let mut remap = vec![0usize; builtins.len() + located.len()];
    for (index, slot) in remap.iter_mut().enumerate().take(builtins.len()) {
        *slot = index;
    }
    for (index, (location, _)) in located.iter().enumerate() {
        let target = rebuilt
            .iter()
            .position(|argument| {
                matches!(&argument.binding,
                    Some(Binding::Location { location: found, .. }) if found == location)
            })
            .ok_or_else(|| format!("fragment input LOC{location} vanished from the rebuild"))?;
        remap[builtins.len() + index] = builtins.len() + target;
    }

    let mut arguments = builtins;
    arguments.extend(rebuilt);
    let function = &mut module.entry_points[entry_index].function;
    function.arguments = arguments;
    for (_, expression) in function.expressions.iter_mut() {
        if let Expression::FunctionArgument(index) = expression
            && let Some(mapped) = remap.get(*index as usize)
        {
            *index = *mapped as u32;
        }
    }
    for (index, literal, components) in padded_arguments.iter().rev() {
        guard_input(function, *index as u32, *literal, *components);
    }
    Ok(padded)
}

/// Prepends a `discard` guard that reads the padded input, keeping it in the compiled signature.
///
/// The range must not cover expressions the validator pre-emits
/// (`Expression::needs_pre_emit`: literals, arguments, globals): listing one of those makes it
/// report `ExpressionAlreadyInScope`. Only the indexed read and the comparison are emitted.
fn guard_input(function: &mut naga::Function, argument: u32, literal: Literal, components: u8) {
    let span = Span::UNDEFINED;
    let argument = function
        .expressions
        .append(Expression::FunctionArgument(argument), span);
    // A scalar has no components to index: `AccessIndex 0` on it is an invalid access.
    let base = if components > 1 {
        function.expressions.append(
            Expression::AccessIndex {
                base: argument,
                index: 0,
            },
            span,
        )
    } else {
        argument
    };
    let value = function
        .expressions
        .append(Expression::Literal(literal), span);
    let condition = function.expressions.append(
        Expression::Binary {
            op: BinaryOperator::Equal,
            left: base,
            right: value,
        },
        span,
    );

    let previous = std::mem::replace(&mut function.body, Block::new());
    let mut body = Block::new();
    let first = if components > 1 { base } else { condition };
    body.push(
        Statement::Emit(Range::new_from_bounds(first, condition)),
        span,
    );
    let mut accept = Block::new();
    accept.push(Statement::Kill, span);
    body.push(
        Statement::If {
            condition,
            accept,
            reject: Block::new(),
        },
        span,
    );
    for (statement, statement_span) in previous.span_into_iter() {
        body.push(statement, statement_span);
    }
    function.body = body;
}

fn type_shape(module: &Module, ty: Handle<Type>) -> Option<Shape> {
    match &module.types[ty].inner {
        TypeInner::Scalar(scalar) => Some((1, scalar.kind)),
        TypeInner::Vector { size, scalar } => Some((size_components(*size), scalar.kind)),
        _ => None,
    }
}

fn size_components(size: VectorSize) -> u8 {
    match size {
        VectorSize::Bi => 2,
        VectorSize::Tri => 3,
        VectorSize::Quad => 4,
    }
}

/// Reuses an existing scalar/vector type, adding one when the module has none.
fn intern_type(module: &mut Module, components: u8, kind: ScalarKind) -> Handle<Type> {
    let scalar = Scalar { kind, width: 4 };
    let wanted = match components {
        1 => TypeInner::Scalar(scalar),
        2 => TypeInner::Vector {
            size: VectorSize::Bi,
            scalar,
        },
        3 => TypeInner::Vector {
            size: VectorSize::Tri,
            scalar,
        },
        _ => TypeInner::Vector {
            size: VectorSize::Quad,
            scalar,
        },
    };
    if let Some(handle) = module
        .types
        .iter()
        .find(|(_, ty)| ty.inner == wanted)
        .map(|(handle, _)| handle)
    {
        return handle;
    }
    module.types.insert(
        Type {
            name: None,
            inner: wanted,
        },
        Span::UNDEFINED,
    )
}

fn literal_for(kind: ScalarKind) -> Literal {
    match kind {
        ScalarKind::Sint => Literal::I32(1234),
        ScalarKind::Uint => Literal::U32(1234),
        _ => Literal::F32(1234.5678),
    }
}

fn parse_interpolation(name: Option<&str>, kind: ScalarKind) -> Option<Interpolation> {
    match name {
        Some("Flat") => Some(Interpolation::Flat),
        Some("Linear") => Some(Interpolation::Linear),
        Some("Perspective") => Some(Interpolation::Perspective),
        _ => Some(match kind {
            ScalarKind::Float => Interpolation::Perspective,
            _ => Interpolation::Flat,
        }),
    }
}

fn components_label(components: u8) -> &'static str {
    match components {
        1 => "",
        2 => "2",
        3 => "3",
        _ => "4",
    }
}

/// Parses an HLSL scalar/vector spelling into a shape.
fn parse_hlsl_type(name: &str) -> Option<Shape> {
    let (kind, width) = if let Some(rest) = name.strip_prefix("float") {
        (ScalarKind::Float, rest)
    } else if let Some(rest) = name.strip_prefix("int") {
        (ScalarKind::Sint, rest)
    } else if let Some(rest) = name.strip_prefix("uint") {
        (ScalarKind::Uint, rest)
    } else {
        (ScalarKind::Float, name.strip_prefix("half")?)
    };
    let components = match width {
        "" => 1,
        "2" => 2,
        "3" => 3,
        "4" => 4,
        _ => return None,
    };
    Some((components, kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fragment_module(source: &str) -> Module {
        naga::front::glsl::Frontend::default()
            .parse(
                &naga::front::glsl::Options::from(ShaderStage::Fragment),
                source,
            )
            .expect("the shader parses")
    }

    fn locations(module: &Module) -> Vec<u32> {
        module.entry_points[0]
            .function
            .arguments
            .iter()
            .filter_map(|argument| match &argument.binding {
                Some(Binding::Location { location, .. }) => Some(*location),
                _ => None,
            })
            .collect()
    }

    fn validate(module: &Module) {
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        validator
            .validate(module)
            .expect("the padded module validates");
    }

    #[test]
    fn pads_the_inputs_the_fragment_stage_skips() {
        let mut module = fragment_module(
            "#version 450\nlayout(location = 1) in vec2 uv;\nlayout(location = 0) out vec4 color;\nvoid main() { color = vec4(uv, 0.0, 1.0); }\n",
        );
        let outputs = vec![
            StageOutput {
                location: 0,
                hlsl_type: "float".to_owned(),
                interpolation: Some("Perspective".to_owned()),
            },
            StageOutput {
                location: 1,
                hlsl_type: "float2".to_owned(),
                interpolation: Some("Perspective".to_owned()),
            },
        ];
        assert_eq!(pad_fragment_inputs(&mut module, "main", &outputs), Ok(1));
        assert_eq!(locations(&module), vec![0, 1]);
        assert_eq!(
            module.entry_points[0].function.arguments[0].name.as_deref(),
            Some("dxr_pad0"),
        );
        // The guard reads a scalar without indexing it; indexing one is an invalid access.
        validate(&module);
    }

    #[test]
    fn a_matching_interface_is_left_alone() {
        let mut module = fragment_module(
            "#version 450\nlayout(location = 0) in vec2 uv;\nlayout(location = 0) out vec4 color;\nvoid main() { color = vec4(uv, 0.0, 1.0); }\n",
        );
        let outputs = vec![StageOutput {
            location: 0,
            hlsl_type: "float2".to_owned(),
            interpolation: None,
        }];
        assert_eq!(pad_fragment_inputs(&mut module, "main", &outputs), Ok(0));
    }
}
