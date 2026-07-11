use a3s_acl::Block;
use anyhow::Result;

#[derive(Clone, Copy)]
pub enum Labels {
    None,
    Exactly(usize),
}

pub struct BlockSchema<'a> {
    pub attributes: &'a [&'a str],
    pub children: &'a [&'a str],
    pub labels: Labels,
}

pub fn validate_block(block: &Block, context: &str, schema: BlockSchema<'_>) -> Result<()> {
    match schema.labels {
        Labels::None => anyhow::ensure!(block.labels.is_empty(), "{context} must not have labels"),
        Labels::Exactly(count) => anyhow::ensure!(
            block.labels.len() == count,
            "{context} must have exactly {count} label(s)"
        ),
    }
    for attribute in block.attributes.keys() {
        anyhow::ensure!(
            schema.attributes.contains(&attribute.as_str()),
            "{context} contains unknown attribute {attribute:?}"
        );
    }
    for child in &block.blocks {
        anyhow::ensure!(
            schema.children.contains(&child.name.as_str()),
            "{context} contains unknown block {:?}",
            child.name
        );
    }
    Ok(())
}

pub fn validate_scalar_block(block: &Block) -> Result<()> {
    validate_block(
        block,
        &block.name,
        BlockSchema {
            attributes: &[block.name.as_str()],
            children: &[],
            labels: Labels::None,
        },
    )?;
    anyhow::ensure!(
        block.attributes.len() == 1,
        "{} must contain exactly one value",
        block.name
    );
    Ok(())
}
