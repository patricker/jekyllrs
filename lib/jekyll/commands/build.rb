# frozen_string_literal: true

module Jekyll
  module Commands
    class Build < Command
      class << self
        # Create the Mercenary command for the Jekyll CLI for this Command
        def init_with_program(prog)
          prog.command(:build) do |c|
            c.syntax      "build [options]"
            c.description "Build your site"
            c.alias :b

            add_build_options(c)

            c.action do |_, options|
              options["serving"] = false
              process_with_graceful_fail(c, options, self)
            end
          end
        end

        # Build your jekyll site
        # Continuously watch if `watch` is set to true in the config.
        def process(options)
          # Hand off entire build flow (verbosity, config, site, logging, watch) to Rust
          Jekyll::Rust.engine_build_process(options)
        end

        # Build your Jekyll site.
        #
        # site - the Jekyll::Site instance to build
        # options - A Hash of options passed to the command
        #
        # Returns nothing.
        def build(site, options)
          # No-op (kept for compatibility); Rust handles build orchestration now.
          Jekyll::Rust.engine_build_site(site)
        end

        # Private: Watch for file changes and rebuild the site.
        #
        # site - A Jekyll::Site instance
        # options - A Hash of options passed to the command
        #
        # Returns nothing.
        def watch(site, options)
          External.require_with_graceful_fail "jekyll-watch"
          Jekyll::Watcher.watch(options, site)
        end
      end
    end
  end
end
