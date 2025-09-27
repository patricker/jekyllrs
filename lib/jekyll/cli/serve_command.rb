# frozen_string_literal: true

module Jekyll
  module CLI
    module ServeCommand
      DEFAULT_LIVERELOAD_PORT = 35_729
      SERVE_OPTION_SPECS = {
        "ssl_cert" => ["--ssl-cert [CERT]", "X.509 (SSL) certificate."],
        "host" => ["host", "-H", "--host [HOST]", "Host to bind to"],
        "open_url" => ["-o", "--[no-]open-url", "Launch your site in a browser"],
        "detach" => ["-B", "--detach", "Run the server in the background"],
        "ssl_key" => ["--ssl-key [KEY]", "X.509 (SSL) Private Key."],
        "port" => ["-P", "--port [PORT]", "Port to listen on"],
        "show_dir_listing" => ["--show-dir-listing", "Show a directory listing instead of loading your index file."],
        "skip_initial_build" => ["skip_initial_build", "--skip-initial-build", "Skips the initial site build which occurs before the server is started."],
        "livereload" => ["-l", "--livereload", "Use LiveReload to automatically refresh browsers"],
        "livereload_ignore" => ["--livereload-ignore GLOB1[,GLOB2[,...]]", Array,
                                 "Files for LiveReload to ignore. Remember to quote the values so your shell won't expand them"],
        "livereload_min_delay" => ["--livereload-min-delay [SECONDS]", "Minimum reload delay"],
        "livereload_max_delay" => ["--livereload-max-delay [SECONDS]", "Maximum reload delay"],
        "livereload_port" => ["--livereload-port [PORT]", Integer, "Port for LiveReload to listen on"],
      }.freeze

      module_function

      def register(program)
        program.command(:serve) do |cmd|
          cmd.description "Serve your site locally"
          cmd.syntax "serve [options]"
          cmd.alias :server
          cmd.alias :s

          Jekyll::Command.add_build_options(cmd)
          SERVE_OPTION_SPECS.each do |key, spec|
            cmd.option(key, *spec)
          end

          cmd.action do |_, opts|
            start(opts, trace: cmd.trace, command_name: cmd.name)
          end
        end
      end

      def start(options, trace: false, command_name: "serve")
        opts = options.dup
        opts["serving"] = true
        opts["watch"] = true unless opts.key?("watch")

        config = Jekyll::Command.configuration_from_options(opts)
        config["serving"] = true
        config["watch"] = true if truthy?(config["watch"]).nil?

        apply_livereload_config(config)

        config["url"] = default_url(config) if Jekyll.env == "development"

        run_build(config, trace: trace, command_name: command_name)
        run_server(config, trace: trace, command_name: command_name)
      end

      def apply_livereload_config(config)
        if truthy?(config["livereload"]) == true
          config["livereload_port"] ||= DEFAULT_LIVERELOAD_PORT
          if truthy?(config["detach"]) == true
            Jekyll.logger.warn "Warning:", "--detach and --livereload are mutually exclusive. Choosing --livereload"
            config["detach"] = false
          end

          if config["ssl_cert"] || config["ssl_key"]
            Jekyll.logger.abort_with "Error:", "LiveReload does not support SSL"
          end

          config["watch"] = true unless truthy?(config["watch"]) == true
        else
          extras = [
            config["livereload_min_delay"],
            config["livereload_max_delay"],
            config["livereload_ignore"],
            config["livereload_port"],
          ]
          if extras.any? { |value| present?(value) }
            Jekyll.logger.abort_with "Error:", "--livereload-min-delay, --livereload-max-delay, --livereload-ignore, and --livereload-port require the --livereload option."
          end
        end
      end

      def present?(value)
        return false if value.nil?
        return !value.empty? if value.respond_to?(:empty?)
        true
      end

      def run_build(config, trace:, command_name:)
        Jekyll::Rust.engine_build_process(config)
      rescue Exception => e
        raise e if trace
        abort_with_trace(command_name)
      end

      def run_server(config, trace:, command_name:)
        Jekyll::Rust.engine_serve_process(config)
      rescue Exception => e
        raise e if trace
        abort_with_trace(command_name)
      end

      def abort_with_trace(command_name)
        msg = " Please append `--trace` to the `#{command_name}` command "
        dashes = "-" * msg.length
        Jekyll.logger.error "", dashes
        Jekyll.logger.error "Jekyll #{Jekyll::VERSION} ", msg
        Jekyll.logger.error "", " for any additional information or backtrace. "
        Jekyll.logger.abort_with "", dashes
      end

      def default_url(config)
        ssl_enabled = truthy?(config["ssl_cert"]) && truthy?(config["ssl_key"])
        host = (config["host"] || "127.0.0.1").dup
        host = "localhost" if host == "127.0.0.1"
        port = config["port"] || 4000
        format_url(ssl_enabled, host, port, config["baseurl"])
      end

      def format_url(ssl_enabled, address, port, baseurl = nil)
        formatted_address = address.include?(":") ? "[#{address}]" : address
        suffix = baseurl && !baseurl.empty? ? "#{baseurl}/" : ""
        format("%<scheme>s://%<address>s:%<port>d%<suffix>s",
               :scheme => ssl_enabled ? "https" : "http",
               :address => formatted_address,
               :port => port.to_i,
               :suffix => suffix)
      end

      def truthy?(value)
        return true if value == true
        return false if value == false
        return nil if value.nil?
        value.respond_to?(:empty?) ? !value.empty? : !!value
      end
    end
  end
end
